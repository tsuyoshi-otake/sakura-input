//! C2 (condition) coverage for the overlay path that `data/conversion-priorities.tsv`
//! travels into the shipped image: the licensed TSV parser and the layer merge.
//!
//! Issue #48 shipped through exactly these two functions, and the mistakes that
//! bit while writing it are decisions inside them -- a dropped trailing tab
//! turning a row into seven columns, and a row that must replace one lattice
//! edge without adding a second. Every atomic condition is therefore driven to
//! both of its outcomes by a named case, and `every_parser_condition_is_driven_both_ways`
//! fails if the matrix ever stops covering one of them.

use std::collections::{BTreeMap, BTreeSet};

use dictc::{
    entries_to_category_tsv, merge_entries, parse_category_entries, parse_entries, SourceEntry,
};
use sakura_core::dictionary::EntryFlags;

const LICENSE: &str = "# license: LicenseRef-Sakura-InHouse\n";
const HEADER: &str =
    "reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n";
/// The exact shape of a row this overlay ships, trailing empty annotation and
/// all.
const DIGIT_ROW: &str = "いち\t1\t2044\t2044\t3639\t4839\tpredict\t\n";
const DIGIT_RENDERED: &str = "いち|1|2044|2044|3639|4839|predict|";

/// Number of parser conditions the matrix below claims to cover. Adding or
/// retiring one is a deliberate edit, not a silent drift.
const PARSER_CONDITIONS: usize = 32;
/// Number of merge conditions, same rule.
const MERGE_CONDITIONS: usize = 12;

/// Which public entry point the case goes through. The two differ only in
/// `require_license`, which is itself one of the conditions under test.
#[derive(Clone, Copy, Debug)]
enum EntryPoint {
    /// `parse_entries`: a licensed source such as the overlay itself.
    Licensed,
    /// `parse_category_entries`: a generated category file, no metadata.
    Category,
}

enum Outcome {
    /// Accepted, rendering exactly these rows.
    Rows(&'static [&'static str]),
    /// Accepted with this row count, where spelling the row out adds nothing.
    RowCount(usize),
    /// Rejected with a message containing this fragment.
    Rejected(&'static str),
}

struct ParseCase {
    condition: &'static str,
    taken: bool,
    entry_point: EntryPoint,
    text: String,
    outcome: Outcome,
}

struct MergeCase {
    condition: &'static str,
    taken: bool,
    system: &'static [&'static str],
    overlay: &'static [&'static str],
    outcome: Outcome,
}

fn licensed(condition: &'static str, taken: bool, text: String, outcome: Outcome) -> ParseCase {
    ParseCase {
        condition,
        taken,
        entry_point: EntryPoint::Licensed,
        text,
        outcome,
    }
}

fn category(condition: &'static str, taken: bool, text: String, outcome: Outcome) -> ParseCase {
    ParseCase {
        condition,
        taken,
        entry_point: EntryPoint::Category,
        text,
        outcome,
    }
}

/// A complete licensed document whose data section is `body`.
fn doc(body: &str) -> String {
    format!("{LICENSE}{HEADER}{body}")
}

fn crlf(text: &str) -> String {
    text.replace('\n', "\r\n")
}

/// One entry as a single comparable line. Costs are printed verbatim, so the
/// `-` prediction column shows up as `i32::MAX` rather than hiding behind the
/// same spelling it had in the source.
fn render(entry: &SourceEntry) -> String {
    let mut flags = String::new();
    for (flag, name) in [
        (EntryFlags::IT, "it"),
        (EntryFlags::PREDICTION, "predict"),
        (EntryFlags::SPELLING_CORRECTION, "correction"),
        (EntryFlags::NON_INITIAL, "non-initial"),
    ] {
        if entry.flags.contains(flag) {
            if !flags.is_empty() {
                flags.push(',');
            }
            flags.push_str(name);
        }
    }
    format!(
        "{}|{}|{}|{}|{}|{}|{}|{}",
        entry.reading,
        entry.surface,
        entry.left_id,
        entry.right_id,
        entry.word_cost,
        entry.prediction_cost,
        flags,
        entry.annotation
    )
}

fn check(label: &str, outcome: &Outcome, parsed: Result<Vec<SourceEntry>, impl ToString>) {
    match (outcome, parsed) {
        (Outcome::Rows(expected), Ok(entries)) => {
            let rendered: Vec<String> = entries.iter().map(render).collect();
            assert_eq!(rendered, *expected, "{label}");
        }
        (Outcome::RowCount(expected), Ok(entries)) => {
            assert_eq!(entries.len(), *expected, "{label}");
        }
        (Outcome::Rejected(fragment), Err(error)) => {
            let message = error.to_string();
            assert!(
                message.contains(*fragment),
                "{label}: expected a message containing '{fragment}', got '{message}'"
            );
        }
        (Outcome::Rows(_) | Outcome::RowCount(_), Err(error)) => {
            let message = error.to_string();
            panic!("{label}: expected acceptance, got '{message}'");
        }
        (Outcome::Rejected(fragment), Ok(entries)) => panic!(
            "{label}: expected rejection containing '{fragment}', got {} accepted rows",
            entries.len()
        ),
    }
}

fn parser_cases() -> Vec<ParseCase> {
    let long_annotation = "a".repeat(1536);
    let over_long_annotation = "a".repeat(1537);
    vec![
        // `line.trim().is_empty()`
        licensed(
            "a blank line is skipped",
            true,
            doc(&format!("\n \t \n{DIGIT_ROW}")),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        licensed(
            "a blank line is skipped",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `line.strip_prefix('#')`
        licensed(
            "a '#' line is metadata, not data",
            true,
            doc(&format!("# a note\n{DIGIT_ROW}")),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        licensed(
            "a '#' line is metadata, not data",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `!require_license` inside the comment branch
        category(
            "a category file rejects every comment",
            true,
            format!("# a note\n{HEADER}{DIGIT_ROW}"),
            Outcome::Rejected("must not contain comments"),
        ),
        licensed(
            "a category file rejects every comment",
            false,
            doc(&format!("# a note\n{DIGIT_ROW}")),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `comment.trim().strip_prefix("license:")`
        licensed(
            "a comment declares the license",
            true,
            format!("# license: MIT\n{HEADER}{DIGIT_ROW}"),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        licensed(
            "a comment declares the license",
            false,
            format!("# licence: MIT\n{HEADER}{DIGIT_ROW}"),
            Outcome::Rejected("must precede data"),
        ),
        // `license.replace(..).is_some()`
        licensed(
            "a second license declaration is a source error",
            true,
            format!("{LICENSE}# license: MIT\n{HEADER}{DIGIT_ROW}"),
            Outcome::Rejected("duplicate license declaration"),
        ),
        licensed(
            "a second license declaration is a source error",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `license.is_none()` before data
        licensed(
            "data before the license is a source error",
            true,
            format!("{HEADER}{DIGIT_ROW}"),
            Outcome::Rejected("must precede data"),
        ),
        licensed(
            "data before the license is a source error",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `require_license`, the other operand of that `&&`
        licensed(
            "only a licensed source demands a license before data",
            true,
            format!("{HEADER}{DIGIT_ROW}"),
            Outcome::Rejected("must precede data"),
        ),
        category(
            "only a licensed source demands a license before data",
            false,
            format!("{HEADER}{DIGIT_ROW}"),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `!saw_header`
        licensed(
            "the first data line must be the header",
            true,
            format!("{LICENSE}{DIGIT_ROW}"),
            Outcome::Rejected("unexpected header"),
        ),
        licensed(
            // Once the header is seen the flag stays set, so a repeated header
            // line is read as data -- and its third column is not a number.
            "the first data line must be the header",
            false,
            doc(&format!("{HEADER}{DIGIT_ROW}")),
            Outcome::Rejected("invalid left_id 'left_id'"),
        ),
        // `line != TSV_HEADER`
        licensed(
            "the header must match the schema exactly",
            true,
            format!("{LICENSE}Reading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n{DIGIT_ROW}"),
            Outcome::Rejected("unexpected header"),
        ),
        licensed(
            "the header must match the schema exactly",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `raw.trim_end_matches('\r')`
        licensed(
            "a CRLF checkout parses like an LF one",
            true,
            crlf(&doc(DIGIT_ROW)),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        licensed(
            "a CRLF checkout parses like an LF one",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `columns.len() != 8`
        licensed(
            "a row has exactly eight columns",
            true,
            doc("いち\t1\t2044\t2044\t3639\t4839\tpredict\n"),
            Outcome::Rejected("expected 8 tab-separated columns, found 7"),
        ),
        licensed(
            "a row has exactly eight columns",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // Issue #49: the split has to be complete. A capped `splitn(8, '\t')`
        // could not fail the count check here -- the ninth column landed in
        // the annotation, which the user reads as a candidate note.
        licensed(
            "a ninth column is counted rather than absorbed",
            true,
            doc("いち\t1\t2044\t2044\t3639\t4839\tpredict\tnote\textra\n"),
            Outcome::Rejected("expected 8 tab-separated columns, found 9"),
        ),
        licensed(
            "a ninth column is counted rather than absorbed",
            false,
            doc("いち\t1\t2044\t2044\t3639\t4839\tpredict\tnote\n"),
            Outcome::Rows(&["いち|1|2044|2044|3639|4839|predict|note"]),
        ),
        // Licensed sources are hand-edited. A `[calibration]` tag in this
        // column ships as a candidate note. Generated category files may still
        // carry a baked tag; dictc strips those after extracting details.
        licensed(
            "a licensed source rejects a bracket tag in the annotation",
            true,
            doc("きのう\t昨日\t1841\t1841\t1100\t2300\tpredict\t[calibration] date expression\n"),
            Outcome::Rejected("annotation must not start with '['"),
        ),
        category(
            "a licensed source rejects a bracket tag in the annotation",
            false,
            format!("{HEADER}きのう\t昨日\t1841\t1841\t1100\t2300\tpredict\t[calibration] date expression\n"),
            Outcome::Rows(&["きのう|昨日|1841|1841|1100|2300|predict|[calibration] date expression"]),
        ),
        // `validate_text(reading)`: emptiness
        licensed(
            "the reading must not be empty",
            true,
            doc("\t1\t2044\t2044\t3639\t4839\tpredict\t\n"),
            Outcome::Rejected("reading must not be empty"),
        ),
        licensed(
            "the reading must not be empty",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `reading.chars().any(char::is_control)`
        licensed(
            "the reading must not carry control characters",
            true,
            doc("い\u{7}ち\t1\t2044\t2044\t3639\t4839\tpredict\t\n"),
            Outcome::Rejected("control characters"),
        ),
        licensed(
            "the reading must not carry control characters",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `validate_text(surface)`: emptiness
        licensed(
            "the surface must not be empty",
            true,
            doc("いち\t\t2044\t2044\t3639\t4839\tpredict\t\n"),
            Outcome::Rejected("surface must not be empty"),
        ),
        licensed(
            "the surface must not be empty",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `field != "annotation"`: only the annotation may be empty
        licensed(
            "only the annotation column may be empty",
            true,
            doc("いち\t\t2044\t2044\t3639\t4839\tpredict\t\n"),
            Outcome::Rejected("surface must not be empty"),
        ),
        licensed(
            "only the annotation column may be empty",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `value.as_bytes().contains(&0)`
        licensed(
            "no field may carry a NUL",
            true,
            doc("いち\t1\t2044\t2044\t3639\t4839\tpredict\tno\u{0}te\n"),
            Outcome::Rejected("annotation contains NUL"),
        ),
        licensed(
            "no field may carry a NUL",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `value.len() > MAX_PREEDIT_BYTES`
        licensed(
            "a field may not exceed the preedit byte budget",
            true,
            doc(&format!(
                "いち\t1\t2044\t2044\t3639\t4839\tpredict\t{over_long_annotation}\n"
            )),
            Outcome::Rejected("exceeds 1536"),
        ),
        licensed(
            "a field may not exceed the preedit byte budget",
            false,
            doc(&format!(
                "いち\t1\t2044\t2044\t3639\t4839\tpredict\t{long_annotation}\n"
            )),
            Outcome::RowCount(1),
        ),
        // `parse_number::<u16>(left_id)`
        licensed(
            "left_id parses as u16",
            true,
            doc("いち\t1\tx\t2044\t3639\t4839\tpredict\t\n"),
            Outcome::Rejected("invalid left_id 'x'"),
        ),
        licensed(
            "left_id parses as u16",
            true,
            doc("いち\t1\t65536\t2044\t3639\t4839\tpredict\t\n"),
            Outcome::Rejected("invalid left_id '65536'"),
        ),
        licensed(
            "left_id parses as u16",
            false,
            doc("いち\t1\t65535\t2044\t3639\t4839\tpredict\t\n"),
            Outcome::Rows(&["いち|1|65535|2044|3639|4839|predict|"]),
        ),
        // `parse_number::<u16>(right_id)`
        licensed(
            "right_id parses as u16",
            true,
            doc("いち\t1\t2044\t65536\t3639\t4839\tpredict\t\n"),
            Outcome::Rejected("invalid right_id '65536'"),
        ),
        licensed(
            "right_id parses as u16",
            false,
            doc("いち\t1\t2044\t65535\t3639\t4839\tpredict\t\n"),
            Outcome::Rows(&["いち|1|2044|65535|3639|4839|predict|"]),
        ),
        // `parse_number::<i32>(word_cost)`
        licensed(
            "word_cost parses as i32",
            true,
            doc("いち\t1\t2044\t2044\tx\t4839\tpredict\t\n"),
            Outcome::Rejected("invalid word_cost 'x'"),
        ),
        licensed(
            "word_cost parses as i32",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `word_cost < 0`
        licensed(
            "word_cost is non-negative",
            true,
            doc("いち\t1\t2044\t2044\t-1\t4839\tpredict\t\n"),
            Outcome::Rejected("word_cost must be non-negative"),
        ),
        licensed(
            "word_cost is non-negative",
            false,
            doc("いち\t1\t2044\t2044\t0\t4839\tpredict\t\n"),
            Outcome::Rows(&["いち|1|2044|2044|0|4839|predict|"]),
        ),
        // `columns[5] == "-"`
        licensed(
            "'-' means the row is not predicted",
            true,
            doc("いち\t1\t2044\t2044\t3639\t-\tpredict\t\n"),
            Outcome::Rows(&["いち|1|2044|2044|3639|2147483647|predict|"]),
        ),
        licensed(
            "'-' means the row is not predicted",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `parse_number::<i32>(prediction_cost)`
        licensed(
            "prediction_cost parses as i32",
            true,
            doc("いち\t1\t2044\t2044\t3639\tx\tpredict\t\n"),
            Outcome::Rejected("invalid prediction_cost 'x'"),
        ),
        licensed(
            "prediction_cost parses as i32",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `cost < 0`
        licensed(
            "prediction_cost is non-negative",
            true,
            doc("いち\t1\t2044\t2044\t3639\t-1\tpredict\t\n"),
            Outcome::Rejected("prediction_cost must be non-negative"),
        ),
        licensed(
            "prediction_cost is non-negative",
            false,
            doc("いち\t1\t2044\t2044\t3639\t0\tpredict\t\n"),
            Outcome::Rows(&["いち|1|2044|2044|3639|0|predict|"]),
        ),
        // `value.is_empty()` in `parse_flags`
        licensed(
            "an empty flags column means no flags",
            true,
            doc("いち\t1\t2044\t2044\t3639\t4839\t\t\n"),
            Outcome::Rows(&["いち|1|2044|2044|3639|4839||"]),
        ),
        licensed(
            "an empty flags column means no flags",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // the `_ =>` arm of `parse_flags`
        licensed(
            "every flag name is known",
            true,
            doc("いち\t1\t2044\t2044\t3639\t4839\tboost\t\n"),
            Outcome::Rejected("unknown flag 'boost'"),
        ),
        licensed(
            "every flag name is known",
            false,
            doc("いち\t1\t2044\t2044\t3639\t4839\tit,predict,correction\t\n"),
            Outcome::Rows(&["いち|1|2044|2044|3639|4839|it,predict,correction|"]),
        ),
        // `flags.contains(parsed)`
        licensed(
            "a flag may not repeat",
            true,
            doc("いち\t1\t2044\t2044\t3639\t4839\tpredict,predict\t\n"),
            Outcome::Rejected("duplicate flag 'predict'"),
        ),
        licensed(
            "a flag may not repeat",
            false,
            doc("いち\t1\t2044\t2044\t3639\t4839\tit,predict\t\n"),
            Outcome::Rows(&["いち|1|2044|2044|3639|4839|it,predict|"]),
        ),
        // `ALLOWED_LICENSES.contains(&license)`
        licensed(
            "the license is on the data allowlist",
            true,
            format!("# license: LicenseRef-Unknown-Proprietary\n{HEADER}{DIGIT_ROW}"),
            Outcome::Rejected("not on the dictionary-data allowlist"),
        ),
        licensed(
            "the license is on the data allowlist",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `license.ok_or_else(..)` after the loop
        licensed(
            "a licensed source ends with a license",
            true,
            "# a note without a license\n".to_owned(),
            Outcome::Rejected("missing license declaration"),
        ),
        licensed(
            "a licensed source ends with a license",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
        // `!saw_header` after the loop
        licensed(
            "the file must contain the header",
            true,
            LICENSE.to_owned(),
            Outcome::Rejected("missing dictionary TSV header"),
        ),
        category(
            "the file must contain the header",
            true,
            String::new(),
            Outcome::Rejected("missing dictionary TSV header"),
        ),
        licensed(
            "the file must contain the header",
            false,
            doc(DIGIT_ROW),
            Outcome::Rows(&[DIGIT_RENDERED]),
        ),
    ]
}

const SYSTEM_A: &str = "あ\tア\t1\t1\t100\t200\tpredict\tsystem";
const SYSTEM_B: &str = "い\tイ\t1\t1\t100\t200\tpredict\tsystem";
const OVERLAY_A: &str = "あ\tア\t1\t1\t900\t1000\tit,predict\toverlay";
const OVERLAY_B: &str = "い\tイ\t1\t1\t900\t1000\tit,predict\toverlay";
const OVERLAY_A_OTHER_READING: &str = "あい\tア\t1\t1\t900\t1000\tit,predict\toverlay";
const OVERLAY_A_OTHER_SURFACE: &str = "あ\t亜\t1\t1\t900\t1000\tit,predict\toverlay";
const OVERLAY_A_OTHER_LEFT: &str = "あ\tア\t2\t1\t900\t1000\tit,predict\toverlay";
const OVERLAY_A_OTHER_RIGHT: &str = "あ\tア\t1\t2\t900\t1000\tit,predict\toverlay";

const SYSTEM_A_RENDERED: &str = "あ|ア|1|1|100|200|predict|system";
const SYSTEM_B_RENDERED: &str = "い|イ|1|1|100|200|predict|system";
const OVERLAY_A_RENDERED: &str = "あ|ア|1|1|900|1000|it,predict|overlay";
const OVERLAY_B_RENDERED: &str = "い|イ|1|1|900|1000|it,predict|overlay";

fn merge_cases() -> Vec<MergeCase> {
    vec![
        MergeCase {
            condition: "the system edge sorts first",
            taken: true,
            system: &[SYSTEM_A],
            overlay: &[OVERLAY_B],
            outcome: Outcome::Rows(&[SYSTEM_A_RENDERED, OVERLAY_B_RENDERED]),
        },
        MergeCase {
            condition: "the system edge sorts first",
            taken: false,
            system: &[SYSTEM_B],
            overlay: &[OVERLAY_A],
            outcome: Outcome::Rows(&[OVERLAY_A_RENDERED, SYSTEM_B_RENDERED]),
        },
        MergeCase {
            condition: "the overlay edge sorts first",
            taken: true,
            system: &[SYSTEM_B],
            overlay: &[OVERLAY_A],
            outcome: Outcome::Rows(&[OVERLAY_A_RENDERED, SYSTEM_B_RENDERED]),
        },
        MergeCase {
            condition: "the overlay edge sorts first",
            taken: false,
            system: &[SYSTEM_A],
            overlay: &[OVERLAY_B],
            outcome: Outcome::Rows(&[SYSTEM_A_RENDERED, OVERLAY_B_RENDERED]),
        },
        MergeCase {
            condition: "the two layers share the edge",
            taken: true,
            system: &[SYSTEM_A],
            overlay: &[OVERLAY_A],
            outcome: Outcome::Rows(&[OVERLAY_A_RENDERED]),
        },
        MergeCase {
            condition: "the two layers share the edge",
            taken: false,
            system: &[SYSTEM_A],
            overlay: &[OVERLAY_B],
            outcome: Outcome::Rows(&[SYSTEM_A_RENDERED, OVERLAY_B_RENDERED]),
        },
        MergeCase {
            condition: "the overlay runs out first",
            taken: true,
            system: &[SYSTEM_A, SYSTEM_B],
            overlay: &[],
            outcome: Outcome::Rows(&[SYSTEM_A_RENDERED, SYSTEM_B_RENDERED]),
        },
        MergeCase {
            condition: "the overlay runs out first",
            taken: false,
            system: &[],
            overlay: &[OVERLAY_A],
            outcome: Outcome::Rows(&[OVERLAY_A_RENDERED]),
        },
        MergeCase {
            condition: "the system runs out first",
            taken: true,
            system: &[],
            overlay: &[OVERLAY_A, OVERLAY_B],
            outcome: Outcome::Rows(&[OVERLAY_A_RENDERED, OVERLAY_B_RENDERED]),
        },
        MergeCase {
            condition: "the system runs out first",
            taken: false,
            system: &[SYSTEM_A],
            overlay: &[],
            outcome: Outcome::Rows(&[SYSTEM_A_RENDERED]),
        },
        MergeCase {
            condition: "both layers end together",
            taken: true,
            system: &[],
            overlay: &[],
            outcome: Outcome::Rows(&[]),
        },
        MergeCase {
            condition: "both layers end together",
            taken: false,
            system: &[SYSTEM_A],
            overlay: &[],
            outcome: Outcome::Rows(&[SYSTEM_A_RENDERED]),
        },
        MergeCase {
            condition: "the reading separates two edges",
            taken: true,
            system: &[SYSTEM_A],
            overlay: &[OVERLAY_A_OTHER_READING],
            outcome: Outcome::Rows(&[SYSTEM_A_RENDERED, "あい|ア|1|1|900|1000|it,predict|overlay"]),
        },
        MergeCase {
            condition: "the reading separates two edges",
            taken: false,
            system: &[SYSTEM_A],
            overlay: &[OVERLAY_A],
            outcome: Outcome::Rows(&[OVERLAY_A_RENDERED]),
        },
        // Issue #48 rests on this one: `いち` must keep both `一` and the
        // re-priced `1`, so a row that differs only in surface adds an edge.
        MergeCase {
            condition: "the surface separates two edges",
            taken: true,
            system: &[SYSTEM_A],
            overlay: &[OVERLAY_A_OTHER_SURFACE],
            outcome: Outcome::Rows(&[SYSTEM_A_RENDERED, "あ|亜|1|1|900|1000|it,predict|overlay"]),
        },
        MergeCase {
            condition: "the surface separates two edges",
            taken: false,
            system: &[SYSTEM_A],
            overlay: &[OVERLAY_A],
            outcome: Outcome::Rows(&[OVERLAY_A_RENDERED]),
        },
        MergeCase {
            condition: "left_id separates two edges",
            taken: true,
            system: &[SYSTEM_A],
            overlay: &[OVERLAY_A_OTHER_LEFT],
            outcome: Outcome::Rows(&[SYSTEM_A_RENDERED, "あ|ア|2|1|900|1000|it,predict|overlay"]),
        },
        MergeCase {
            condition: "left_id separates two edges",
            taken: false,
            system: &[SYSTEM_A],
            overlay: &[OVERLAY_A],
            outcome: Outcome::Rows(&[OVERLAY_A_RENDERED]),
        },
        MergeCase {
            condition: "right_id separates two edges",
            taken: true,
            system: &[SYSTEM_A],
            overlay: &[OVERLAY_A_OTHER_RIGHT],
            outcome: Outcome::Rows(&[SYSTEM_A_RENDERED, "あ|ア|1|2|900|1000|it,predict|overlay"]),
        },
        MergeCase {
            condition: "right_id separates two edges",
            taken: false,
            system: &[SYSTEM_A],
            overlay: &[OVERLAY_A],
            outcome: Outcome::Rows(&[OVERLAY_A_RENDERED]),
        },
        MergeCase {
            condition: "the system layer repeats an edge",
            taken: true,
            system: &[SYSTEM_A, SYSTEM_A],
            overlay: &[OVERLAY_A],
            outcome: Outcome::Rejected("duplicate system entry"),
        },
        MergeCase {
            condition: "the system layer repeats an edge",
            taken: false,
            system: &[SYSTEM_A, SYSTEM_B],
            overlay: &[OVERLAY_A],
            outcome: Outcome::Rows(&[OVERLAY_A_RENDERED, SYSTEM_B_RENDERED]),
        },
        MergeCase {
            condition: "the overlay layer repeats an edge",
            taken: true,
            system: &[SYSTEM_A],
            overlay: &[OVERLAY_A, OVERLAY_A],
            outcome: Outcome::Rejected("duplicate overlay entry"),
        },
        MergeCase {
            condition: "the overlay layer repeats an edge",
            taken: false,
            system: &[SYSTEM_A],
            overlay: &[OVERLAY_A, OVERLAY_B],
            outcome: Outcome::Rows(&[OVERLAY_A_RENDERED, OVERLAY_B_RENDERED]),
        },
    ]
}

fn layer(rows: &[&str]) -> Vec<SourceEntry> {
    let mut text = String::from(LICENSE);
    text.push_str(HEADER);
    for row in rows {
        text.push_str(row);
        text.push('\n');
    }
    parse_entries("layer.tsv", &text).expect("every merge fixture row is well formed")
}

#[test]
fn every_parser_condition_reaches_its_documented_outcome() {
    for case in parser_cases() {
        let label = format!(
            "{} ({})",
            case.condition,
            if case.taken { "taken" } else { "not taken" }
        );
        let parsed = match case.entry_point {
            EntryPoint::Licensed => parse_entries("case.tsv", &case.text),
            EntryPoint::Category => parse_category_entries("case.tsv", &case.text),
        };
        check(&label, &case.outcome, parsed);
    }
}

#[test]
fn every_merge_condition_reaches_its_documented_outcome() {
    for case in merge_cases() {
        let label = format!(
            "{} ({})",
            case.condition,
            if case.taken { "taken" } else { "not taken" }
        );
        let merged = merge_entries(layer(case.system), layer(case.overlay));
        check(&label, &case.outcome, merged);
    }
}

#[test]
fn every_parser_condition_is_driven_both_ways() {
    assert_both_polarities(
        parser_cases()
            .iter()
            .map(|case| (case.condition, case.taken)),
        PARSER_CONDITIONS,
        "parser",
    );
}

#[test]
fn every_merge_condition_is_driven_both_ways() {
    assert_both_polarities(
        merge_cases()
            .iter()
            .map(|case| (case.condition, case.taken)),
        MERGE_CONDITIONS,
        "merge",
    );
}

fn assert_both_polarities<'a>(
    cases: impl Iterator<Item = (&'a str, bool)>,
    expected: usize,
    label: &str,
) {
    let mut polarities: BTreeMap<&str, BTreeSet<bool>> = BTreeMap::new();
    for (condition, taken) in cases {
        polarities.entry(condition).or_default().insert(taken);
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

/// Issue #49. A row with a ninth column used to be accepted, with everything
/// past the eighth column folded into the annotation -- the column the user
/// reads as a candidate note. The writer refused such a row, which made the
/// hole look contained, but the dictionary build compiles parsed entries
/// directly and never passes through that writer. Both entry points now reject
/// the row instead, at any width.
#[test]
fn a_row_wider_than_the_schema_is_rejected_by_both_entry_points() {
    let body = "いち\t1\t2044\t2044\t3639\t4839\tpredict\tnote\textra\n";
    for (label, parsed) in [
        ("licensed", parse_entries("ninth.tsv", &doc(body))),
        (
            "category",
            parse_category_entries("ninth.tsv", &format!("{HEADER}{body}")),
        ),
    ] {
        match parsed {
            Ok(entries) => panic!(
                "{label}: a ninth column was absorbed instead of counted, annotation {:?}",
                entries[0].annotation
            ),
            Err(error) => {
                let message = error.to_string();
                assert!(
                    message.contains("expected 8 tab-separated columns, found 9"),
                    "{label}: unexpected rejection '{message}'"
                );
            }
        }
    }
    // The same row one column narrower is ordinary data, so the check is a
    // width check rather than a ban on the word `extra`.
    let entries = parse_entries(
        "eighth.tsv",
        &doc("いち\t1\t2044\t2044\t3639\t4839\tpredict\tnote\n"),
    )
    .expect("an eight-column row still parses");
    assert_eq!(entries[0].annotation, "note");
    assert!(entries_to_category_tsv(&entries).is_ok());
}
