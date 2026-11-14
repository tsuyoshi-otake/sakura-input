//! Accuracy gate (#333): conversion and prediction against the committed
//! `corpus/accuracy/` corpora, compared row by row with a committed snapshot.
//!
//! ```text
//! accuracy-gate --dictionary <system.dic>            measure and report
//! accuracy-gate --dictionary <system.dic> --check    also fail on a regression
//! accuracy-gate --dictionary <system.dic> --accept   rewrite snapshots and floors
//! ```
//!
//! The measurement is the first conversion of a new session: the product's
//! default options (768 candidates), no learning, no reranker, no context, the
//! top candidate only; prediction types the reading one character at a time
//! with IT ratio 0 and no learning. It reads the dictionary and the corpus
//! only. `corpus/accuracy/README.md` describes the workflow.

#[path = "accuracy_gate/compare.rs"]
mod compare;
#[path = "accuracy_gate/corpus.rs"]
mod corpus;
#[path = "accuracy_gate/counts.rs"]
mod counts;
#[path = "accuracy_gate/measure.rs"]
mod measure;
#[path = "accuracy_gate/replay.rs"]
mod replay;
#[path = "accuracy_gate/report.rs"]
mod report;
#[path = "accuracy_gate/snapshot.rs"]
mod snapshot;

use std::path::{Path, PathBuf};
use std::time::Instant;

use sakura_core::ConversionOptions;
use sha2::{Digest, Sha256};

use compare::{CheckInputs, ContextCheckInputs, ContextDiff, Diff};
use corpus::Corpus;
use counts::{ContextCounts, ContextFloors, Floors, Tally};
use measure::{ContextOutcome, ConversionOutcome, PredictionOutcome};
use report::{ContextReport, ReportInput};
use snapshot::AcceptedBreak;

const CONVERSION_FILE: &str = "conversion.tsv";
const PREDICTION_FILE: &str = "prediction.tsv";
const CONTEXT_FILE: &str = "context.tsv";
const CONVERSION_SNAPSHOT: &str = "conversion.snapshot.tsv";
const PREDICTION_SNAPSHOT: &str = "prediction.snapshot.tsv";
const CONTEXT_SNAPSHOT: &str = "context.snapshot.tsv";
const FLOORS_FILE: &str = "floors.tsv";
const CONTEXT_FLOORS: &str = "context-floors.tsv";
const BREAKS_FILE: &str = "accepted-breaks.tsv";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Measure,
    Check,
    Accept,
}

#[derive(Debug)]
struct Options {
    dictionary: PathBuf,
    corpus_dir: PathBuf,
    build_report: PathBuf,
    report: Option<PathBuf>,
    mode: Mode,
    /// `--history-replay <file>`: replay an outside-repo export, counts only.
    history_replay: Option<PathBuf>,
}

fn main() {
    match run() {
        Ok(true) => {}
        Ok(false) => std::process::exit(1),
        Err(error) => {
            eprintln!("accuracy-gate: {error}");
            std::process::exit(2);
        }
    }
}

fn parse_options(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut dictionary = None;
    let mut corpus_dir = PathBuf::from("corpus/accuracy");
    let mut build_report = PathBuf::from("data/dictionary-build.report.json");
    let mut report = None;
    let mut mode = Mode::Measure;
    let mut history_replay = None;
    let mut args = args;
    while let Some(argument) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match argument.as_str() {
            "--dictionary" => dictionary = Some(PathBuf::from(value("--dictionary")?)),
            "--corpus-dir" => corpus_dir = PathBuf::from(value("--corpus-dir")?),
            "--build-report" => build_report = PathBuf::from(value("--build-report")?),
            "--report" => report = Some(PathBuf::from(value("--report")?)),
            "--history-replay" => history_replay = Some(PathBuf::from(value("--history-replay")?)),
            "--check" | "--accept" => {
                let requested = if argument == "--check" {
                    Mode::Check
                } else {
                    Mode::Accept
                };
                if mode != Mode::Measure && mode != requested {
                    return Err("--check and --accept cannot be combined".to_owned());
                }
                mode = requested;
            }
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    Ok(Options {
        dictionary: dictionary.ok_or("--dictionary <system.dic> is required")?,
        corpus_dir,
        build_report,
        report,
        mode,
        history_replay,
    })
}

/// Lower-case hexadecimal SHA-256 of `bytes`, as the build reports write it.
fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn read_text(path: &Path) -> Result<String, String> {
    String::from_utf8(read_bytes(path)?).map_err(|_| format!("{} is not UTF-8", path.display()))
}

fn read_optional(path: &Path) -> Result<Option<String>, String> {
    if path.exists() {
        read_text(path).map(Some)
    } else {
        Ok(None)
    }
}

/// The SHA-256 `data/dictionary-build.report.json` records for `system.dic`.
fn recorded_dictionary_sha(report: &serde_json::Value) -> Option<String> {
    match report {
        serde_json::Value::Object(map) => {
            if map.get("file").and_then(|file| file.as_str()) == Some("system.dic") {
                if let Some(sha) = map.get("sha256").and_then(|sha| sha.as_str()) {
                    return Some(sha.to_owned());
                }
            }
            map.values().find_map(recorded_dictionary_sha)
        }
        serde_json::Value::Array(items) => items.iter().find_map(recorded_dictionary_sha),
        _ => None,
    }
}

fn dictionary_header(options: &Options, sha: &str) -> Vec<String> {
    let recorded = read_optional(&options.build_report)
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|value| recorded_dictionary_sha(&value));
    let relation = match recorded {
        Some(recorded) if recorded == sha => "equals the recorded system.dic hash".to_owned(),
        Some(recorded) => format!("DIFFERS from the recorded system.dic hash {recorded}"),
        None => "no recorded system.dic hash found".to_owned(),
    };
    vec![format!(
        "dictionary: sha256 {sha} ({relation} in {})",
        options.build_report.display()
    )]
}

struct Stored {
    conversion: Vec<ConversionOutcome>,
    prediction: Vec<PredictionOutcome>,
    /// `None` when the corpus directory has no context snapshot; the vector
    /// stays empty when there is no context corpus either.
    context: Option<Vec<ContextOutcome>>,
}

fn load_snapshots(dir: &Path, corpus: &Corpus) -> Result<Option<Stored>, String> {
    let conversion = read_optional(&dir.join(CONVERSION_SNAPSHOT))?;
    let prediction = read_optional(&dir.join(PREDICTION_SNAPSHOT))?;
    let context = read_optional(&dir.join(CONTEXT_SNAPSHOT))?;
    let context = match context {
        Some(_) if corpus.context.is_empty() => {
            return Err(format!(
                "{CONTEXT_SNAPSHOT} exists but {CONTEXT_FILE} does not"
            ))
        }
        Some(text) => Some(snapshot::parse_context(corpus, &text)?),
        None => None,
    };
    match (conversion, prediction) {
        (None, None) => Ok(None),
        (Some(conversion), Some(prediction)) => Ok(Some(Stored {
            conversion: snapshot::parse_conversion(corpus, &conversion)?,
            prediction: snapshot::parse_prediction(corpus, &prediction)?,
            context,
        })),
        _ => Err(format!(
            "{CONVERSION_SNAPSHOT} and {PREDICTION_SNAPSHOT} must exist together"
        )),
    }
}

fn run() -> Result<bool, String> {
    let options = parse_options(std::env::args().skip(1))?;
    let dir = &options.corpus_dir;

    let conversion_bytes = read_bytes(&dir.join(CONVERSION_FILE))?;
    let prediction_bytes = read_bytes(&dir.join(PREDICTION_FILE))?;
    let text = |bytes: &[u8], file: &str| {
        std::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| format!("{file} is not UTF-8"))
    };
    let context_text = read_optional(&dir.join(CONTEXT_FILE))?;
    let corpus = Corpus {
        conversion: corpus::parse_conversion(&text(&conversion_bytes, CONVERSION_FILE)?)?,
        prediction: corpus::parse_prediction(&text(&prediction_bytes, PREDICTION_FILE)?)?,
        context: match &context_text {
            Some(source) => corpus::parse_context(source)?,
            None => Vec::new(),
        },
    };
    let stored = load_snapshots(dir, &corpus)?;
    let floors_text = read_optional(&dir.join(FLOORS_FILE))?;
    let context_floors_text = read_optional(&dir.join(CONTEXT_FLOORS))?;
    let breaks_text = read_optional(&dir.join(BREAKS_FILE))?;
    if options.mode == Mode::Check
        && (stored.is_none() || floors_text.is_none() || breaks_text.is_none())
    {
        return Err(format!(
            "--check needs {CONVERSION_SNAPSHOT}, {PREDICTION_SNAPSHOT}, {FLOORS_FILE} and {BREAKS_FILE} in {}; run --accept once",
            dir.display()
        ));
    }
    let context_snapshot = stored.as_ref().and_then(|stored| stored.context.as_deref());
    if options.mode == Mode::Check
        && !corpus.context.is_empty()
        && (context_snapshot.is_none() || context_floors_text.is_none())
    {
        return Err(format!(
            "--check needs {CONTEXT_SNAPSHOT} and {CONTEXT_FLOORS} in {}; run --accept once",
            dir.display()
        ));
    }
    let breaks: Vec<AcceptedBreak> = match &breaks_text {
        Some(text) => snapshot::parse_breaks(text)?,
        None => Vec::new(),
    };
    let floors = floors_text
        .as_deref()
        .map(counts::parse_floors)
        .transpose()?;
    let context_floors = context_floors_text
        .as_deref()
        .map(counts::parse_context_floors)
        .transpose()?;

    let dictionary_sha = hex_digest(&read_bytes(&options.dictionary)?);
    let service = sakura_engine::dictionary::open(&options.dictionary)
        .map_err(|error| format!("{}: {error}", options.dictionary.display()))?;

    // `--history-replay` measures an outside-repo developer-history export
    // and reports counts only (#471 phase 1, verify step 5).
    if let Some(path) = &options.history_replay {
        let counts = replay::replay_history(&service, &read_text(path)?)?;
        println!("history replay (counts only; no readings or surfaces leave this run):");
        println!("  normal-scope commits  : {}", counts.commits);
        println!("  replayed with context : {}", counts.replayed);
        println!("  top = committed       : {}", counts.top_match);
        println!("  top != committed      : {}", counts.top_mismatch);
        println!(
            "  of which homophone swap (committed surface among the candidates): {}",
            counts.homophone_swap
        );
        println!("  skipped (no carry)    : {}", counts.skipped);
        return Ok(true);
    }

    let started = Instant::now();
    let conversion = measure::measure_conversion(&service, &corpus.conversion)?;
    let conversion_time = started.elapsed();
    let started = Instant::now();
    let prediction = measure::measure_prediction(&service, &corpus.prediction)?;
    let prediction_time = started.elapsed();
    let started = Instant::now();
    let context = measure::measure_context(&service, &corpus.context)?;
    let context_time = started.elapsed();

    let after = counts::tally(&corpus, &conversion, &prediction);
    let before: Option<Tally> = stored
        .as_ref()
        .map(|stored| counts::tally(&corpus, &stored.conversion, &stored.prediction));
    let diff = stored.as_ref().map_or_else(Diff::default, |stored| {
        compare::diff(
            &stored.conversion,
            &conversion,
            &stored.prediction,
            &prediction,
        )
    });
    let context_after = counts::tally_context(&corpus, &context);
    let context_before: Option<ContextCounts> =
        context_snapshot.map(|snapshot| counts::tally_context(&corpus, snapshot));
    let context_diff = context_snapshot.map_or_else(ContextDiff::default, |snapshot| {
        compare::diff_context(snapshot, &context)
    });

    let mut header = dictionary_header(&options, &dictionary_sha);
    header.push(format!(
        "corpus: {CONVERSION_FILE} sha256 {}, {PREDICTION_FILE} sha256 {}",
        hex_digest(&conversion_bytes),
        hex_digest(&prediction_bytes)
    ));
    header.push(format!(
        "options: max_candidates = {} (ConversionOptions::default()), top candidate only; prediction {} suggestions, IT ratio 0; learning, reranker and context are not measured",
        ConversionOptions::default().max_candidates,
        sakura_engine::prediction::MAX_SUGGESTIONS
    ));
    header.push(format!("mode: {:?}", options.mode).to_lowercase());

    let mut violations = match (options.mode, &before, &floors) {
        (Mode::Check, Some(snapshot_tally), Some(floors)) => Some(compare::check(&CheckInputs {
            corpus: &corpus,
            diff: &diff,
            breaks: &breaks,
            floors,
            measured_counts: &after.counts,
            measured_conversion: &conversion,
            measured_prediction: &prediction,
            snapshot_counts: &snapshot_tally.counts,
        })),
        _ => None,
    };
    if let (Mode::Check, Some(_), Some(floors), Some(snapshot_counts)) = (
        options.mode,
        context_snapshot,
        &context_floors,
        context_before.as_ref(),
    ) {
        violations
            .get_or_insert_with(Vec::new)
            .extend(compare::check_context(&ContextCheckInputs {
                corpus: &corpus,
                diff: &context_diff,
                breaks: &breaks,
                floors,
                measured_counts: &context_after,
                measured_context: &context,
                snapshot_counts,
            }));
    }

    let mut unaccepted: Vec<(snapshot::BreakKind, &str)> = if options.mode == Mode::Accept {
        diff.unaccepted(&corpus, &breaks)
    } else {
        Vec::new()
    };
    if options.mode == Mode::Accept {
        for change in context_diff
            .changed
            .iter()
            .filter(|change| change.is_worse())
        {
            let id = corpus.context[change.index].id.as_str();
            if !breaks
                .iter()
                .any(|entry| entry.kind == snapshot::BreakKind::Context && entry.id == id)
            {
                unaccepted.push((snapshot::BreakKind::Context, id));
            }
        }
    }

    let rendered = report::render(&ReportInput {
        header,
        corpus: &corpus,
        before: before.as_ref(),
        after: &after,
        diff: &diff,
        measured_prediction: &prediction,
        snapshot_prediction: stored.as_ref().map(|stored| stored.prediction.as_slice()),
        violations: violations.as_deref(),
        context: (!corpus.context.is_empty()).then(|| ContextReport {
            before: context_before.as_ref(),
            after: &context_after,
            diff: &context_diff,
            pairs: corpus
                .context
                .iter()
                .map(|row| &row.pair)
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            bridged: context.iter().filter(|outcome| outcome.bridge).count(),
            attempted: context
                .iter()
                .filter(|outcome| outcome.bridge_attempted)
                .count(),
        }),
        timings: vec![format!(
            "runtime: conversion {:.1}s, prediction {:.1}s, context {:.1}s",
            conversion_time.as_secs_f64(),
            prediction_time.as_secs_f64(),
            context_time.as_secs_f64()
        )],
    });
    print!("{rendered}");
    if let Some(path) = &options.report {
        std::fs::write(path, &rendered)
            .map_err(|error| format!("write {}: {error}", path.display()))?;
    }

    match options.mode {
        Mode::Measure => Ok(true),
        Mode::Check => Ok(violations.is_some_and(|violations| violations.is_empty())),
        Mode::Accept => {
            if !unaccepted.is_empty() {
                println!("\naccept: REFUSED; these worsened rows are not in {BREAKS_FILE}:");
                for (kind, id) in unaccepted {
                    println!("  {} {id}", kind.name());
                }
                return Ok(false);
            }
            let mut snapshots = vec![
                (
                    CONVERSION_SNAPSHOT,
                    snapshot::render_conversion(&corpus, &conversion)?,
                ),
                (
                    PREDICTION_SNAPSHOT,
                    snapshot::render_prediction(&corpus, &prediction),
                ),
                (
                    FLOORS_FILE,
                    counts::render_floors(&Floors::from_counts(&after.counts)),
                ),
            ];
            if !corpus.context.is_empty() {
                snapshots.push((
                    CONTEXT_SNAPSHOT,
                    snapshot::render_context(&corpus, &context)?,
                ));
                snapshots.push((
                    CONTEXT_FLOORS,
                    counts::render_context_floors(&ContextFloors::from_counts(&context_after)),
                ));
            }
            for (file, content) in snapshots {
                let path = dir.join(file);
                std::fs::write(&path, content)
                    .map_err(|error| format!("write {}: {error}", path.display()))?;
            }
            if corpus.context.is_empty() {
                println!(
                    "\naccept: wrote {CONVERSION_SNAPSHOT}, {PREDICTION_SNAPSHOT} and {FLOORS_FILE}"
                );
            } else {
                println!(
                    "\naccept: wrote {CONVERSION_SNAPSHOT}, {PREDICTION_SNAPSHOT}, {FLOORS_FILE}, {CONTEXT_SNAPSHOT} and {CONTEXT_FLOORS}"
                );
            }
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> impl Iterator<Item = String> {
        list.iter()
            .map(|argument| (*argument).to_owned())
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn options_require_a_dictionary_and_reject_conflicting_modes() {
        assert!(parse_options(args(&[])).is_err());
        assert!(parse_options(args(&["--dictionary", "d", "--check", "--accept"])).is_err());
        let options = parse_options(args(&["--dictionary", "d", "--check"])).expect("options");
        assert_eq!(options.mode, Mode::Check);
        assert!(parse_options(args(&["--dictionary"])).is_err());
        assert!(parse_options(args(&["--dictionary", "d", "--fast"])).is_err());
    }

    #[test]
    fn finds_the_recorded_dictionary_hash_inside_the_build_report() {
        let report = serde_json::json!({
            "outputs": {"other": {"file": "wordnet.report.json", "sha256": "aa"},
                        "dictionary": {"file": "system.dic", "bytes": 1, "sha256": "bb"}}
        });
        assert_eq!(recorded_dictionary_sha(&report).as_deref(), Some("bb"));
        assert_eq!(
            recorded_dictionary_sha(&serde_json::json!({"file": "x"})),
            None
        );
    }
}
