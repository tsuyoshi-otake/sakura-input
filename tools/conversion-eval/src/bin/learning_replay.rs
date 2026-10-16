//! Replays the developer input history against the engine's learning, once
//! as the engine before #305 and once with correction learning, and reports
//! how often each puts the user's final choice first.
//!
//! Inputs are the plaintext exports of `sakura_settings history export` and
//! `sakura_settings learning export`, plus a built `system.dic`. Learning
//! older than the first history day seeds both replays; every later commit is
//! learned again from the history, so the replay never sees its own future.
//!
//! The replay uses the engine's own correction trail, staged learning, ranking
//! and first-candidate choice (`sakura-engine` feature `replay-eval`). The
//! conversion cache, the reranker, the user dictionary and host events the
//! history does not record (caret moves, document resets) are not replayed.
//!
//! The report holds counts only. `--examples` adds the readings and surfaces
//! of the differing conversions for local inspection; never publish them.

use std::collections::BTreeMap;
use std::path::PathBuf;

use sakura_core::{ConversionOptions, Converter, Dictionary};
use sakura_engine::replay_eval::{ReplayCommit, ReplayLearning, ReplayTrail};

/// `KeyCode::Backspace` as the history records it.
const KEY_BACKSPACE: u16 = 5;
const MODIFIER_CTRL: u8 = 0x02;
const MODIFIER_ALT: u8 = 0x04;
/// `state_code` in the engine's dispatcher.
const STATE_IDLE: u8 = 0;
const STATE_COMPOSING: u8 = 1;
const STATE_CONVERTING: u8 = 2;
const STATE_PREDICTING: u8 = 3;
const DAY_MS: u64 = 86_400_000;
/// The first engine whose history records the conversion's first candidate
/// the way this replay reads it.
const FIDELITY_SINCE: (u32, u32, u32) = (2, 0, 13);
/// Columns of one history row (`history_present.rs`).
const HISTORY_COLUMNS: usize = 40;
const RELEASE_FIDELITY_PERCENT: u64 = 80;

struct Options {
    history: PathBuf,
    learning: PathBuf,
    dictionary: PathBuf,
    examples: bool,
}

fn main() {
    let result = parse_options(std::env::args_os().skip(1)).and_then(|options| {
        let history = read_text(&options.history)?;
        let learning = read_text(&options.learning)?;
        let dictionary_bytes = std::fs::read(&options.dictionary)
            .map_err(|error| format!("{}: {error}", options.dictionary.display()))?;
        let dictionary = Dictionary::parse(&dictionary_bytes)
            .map_err(|error| format!("{}: {error:?}", options.dictionary.display()))?;
        let rows = parse_history(&history)?;
        let records = parse_learning(&learning)?;
        let replay = replay(&dictionary, &rows, &records)?;
        print!("{}", replay.report());
        if options.examples {
            eprintln!("# examples are personal input: never publish them");
            print!("{}", replay.examples());
        }
        Ok(())
    });
    if let Err(error) = result {
        eprintln!("learning-replay: {error}");
        std::process::exit(2);
    }
}

fn parse_options(args: impl Iterator<Item = std::ffi::OsString>) -> Result<Options, String> {
    let mut history = None;
    let mut learning = None;
    let mut dictionary = None;
    let mut examples = false;
    let mut args = args.peekable();
    while let Some(argument) = args.next() {
        let Some(name) = argument.to_str() else {
            return Err("arguments must be valid Unicode".to_owned());
        };
        if matches!(name, "--help" | "-h") {
            println!(
                "Usage: learning-replay --history FILE --learning FILE --dictionary FILE [--examples]"
            );
            std::process::exit(0);
        }
        if name == "--examples" {
            examples = true;
            continue;
        }
        let value = args
            .next()
            .ok_or_else(|| format!("{name} requires a value"))?
            .into_string()
            .map_err(|_| format!("{name} value must be valid Unicode"))?;
        let slot = match name {
            "--history" => &mut history,
            "--learning" => &mut learning,
            "--dictionary" => &mut dictionary,
            _ => return Err(format!("unknown argument '{name}'")),
        };
        if slot.replace(PathBuf::from(value)).is_some() {
            return Err(format!("{name} was specified more than once"));
        }
    }
    Ok(Options {
        history: history.ok_or("--history is required")?,
        learning: learning.ok_or("--learning is required")?,
        dictionary: dictionary.ok_or("--dictionary is required")?,
        examples,
    })
}

fn read_text(path: &std::path::Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

/// Reverses `history_present::escape` and the learning export's escaping.
fn unescape(field: &str) -> String {
    let mut output = String::with_capacity(field.len());
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            output.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => output.push('\t'),
            Some('r') => output.push('\r'),
            Some('n') => output.push('\n'),
            Some(other) => output.push(other),
            None => output.push('\\'),
        }
    }
    output
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KeyRow {
    key_code: u16,
    modifiers: u8,
    consumed: bool,
    state_before: u8,
    state_after: u8,
    preedit_before: String,
    preedit_after: String,
    delete_before: u16,
    action: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommitRow {
    reading: String,
    surface: String,
    left_context: u16,
    right_context: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Key(KeyRow),
    Commit(CommitRow),
    AiText,
    Engine(Option<(u32, u32, u32)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HistoryRow {
    sequence: u64,
    day: u32,
    session: u64,
    scope: String,
    event: Event,
}

fn number<T: std::str::FromStr>(fields: &[String], index: usize, line: usize) -> Result<T, String> {
    fields[index]
        .parse()
        .map_err(|_| format!("history line {line}: column {index} is not a number"))
}

fn flag(fields: &[String], index: usize, line: usize) -> Result<bool, String> {
    match fields[index].as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!(
            "history line {line}: column {index} is not a boolean"
        )),
    }
}

fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let mut parts = text.split('.').map(str::parse::<u32>);
    let version = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    parts.next().is_none().then_some(version)
}

fn parse_history(text: &str) -> Result<Vec<HistoryRow>, String> {
    let mut rows = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        if line.is_empty() || line.starts_with('#') || line.starts_with("kind\t") {
            continue;
        }
        let fields: Vec<String> = line.split('\t').map(unescape).collect();
        if fields.len() != HISTORY_COLUMNS {
            return Err(format!(
                "history line {line_number}: {} columns, expected {HISTORY_COLUMNS}",
                fields.len()
            ));
        }
        let timestamp_ms: u64 = number(&fields, 2, line_number)?;
        let event = match fields[0].as_str() {
            "key" => Event::Key(KeyRow {
                key_code: number(&fields, 5, line_number)?,
                modifiers: number(&fields, 7, line_number)?,
                consumed: flag(&fields, 9, line_number)?,
                state_before: number(&fields, 10, line_number)?,
                state_after: number(&fields, 11, line_number)?,
                preedit_before: fields[14].clone(),
                preedit_after: fields[15].clone(),
                delete_before: number(&fields, 17, line_number)?,
                action: fields[19].clone(),
            }),
            "commit" => Event::Commit(CommitRow {
                reading: fields[21].clone(),
                surface: fields[22].clone(),
                left_context: number(&fields, 23, line_number)?,
                right_context: number(&fields, 24, line_number)?,
            }),
            "ai-text" => Event::AiText,
            "engine" => Event::Engine(parse_version(&fields[38])),
            other => {
                return Err(format!(
                    "history line {line_number}: unknown kind '{other}'"
                ))
            }
        };
        rows.push(HistoryRow {
            sequence: number(&fields, 1, line_number)?,
            day: u32::try_from(timestamp_ms / DAY_MS).unwrap_or(u32::MAX),
            session: number(&fields, 3, line_number)?,
            scope: fields[4].clone(),
            event,
        });
    }
    rows.sort_by_key(|row| row.sequence);
    Ok(rows)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LearningRow {
    sequence: u64,
    day: u32,
    left_context: u16,
    right_context: u16,
    reading: String,
    surface: String,
    rejection: bool,
}

fn parse_learning(text: &str) -> Result<Vec<LearningRow>, String> {
    let mut rows = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;
        let (rejection, body) = match line.strip_prefix("#reject\t") {
            Some(body) => (true, body),
            None if line.is_empty() || line.starts_with('#') || line.starts_with("sequence\t") => {
                continue;
            }
            None => (false, line),
        };
        let fields: Vec<String> = body.split('\t').map(unescape).collect();
        if fields.len() != 6 {
            return Err(format!(
                "learning line {line_number}: {} columns, expected 6",
                fields.len()
            ));
        }
        let parse = |index: usize| {
            fields[index]
                .parse::<u64>()
                .map_err(|_| format!("learning line {line_number}: column {index} is not a number"))
        };
        let narrow = |value: u64, index: usize| {
            u16::try_from(value)
                .map_err(|_| format!("learning line {line_number}: column {index} is out of range"))
        };
        rows.push(LearningRow {
            sequence: parse(0)?,
            day: u32::try_from(parse(1)?)
                .map_err(|_| format!("learning line {line_number}: day is out of range"))?,
            left_context: narrow(parse(2)?, 2)?,
            right_context: narrow(parse(3)?, 3)?,
            reading: fields[4].clone(),
            surface: fields[5].clone(),
            rejection,
        });
    }
    rows.sort_by_key(|row| row.sequence);
    Ok(rows)
}

/// The first candidate one replay selected for a conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice {
    surface: String,
    learned: bool,
}

/// One conversion the user committed, judged after the whole replay.
#[derive(Debug, Clone)]
struct Conversion {
    reading: String,
    committed: String,
    /// The committed surface, or the surface a later correction retyped.
    final_choice: String,
    before: Choice,
    after: Choice,
}

#[derive(Debug, Clone)]
struct PendingConversion {
    reading: String,
    first_candidate: String,
    engine: Option<(u32, u32, u32)>,
}

#[derive(Debug, Default)]
struct SessionReplay {
    trail: ReplayTrail,
    scope: String,
    last_commit: Option<String>,
    pending: Option<PendingConversion>,
    transformed: bool,
    /// Indices into `Replay::conversions` of this session, oldest first.
    conversions: Vec<usize>,
}

#[derive(Debug, Default)]
struct Replay {
    history_rows: usize,
    history_days: usize,
    seeded: usize,
    seeded_rejections: usize,
    commits: usize,
    same_surface_retypes: usize,
    corrections: usize,
    fidelity_conversions: usize,
    fidelity_matches: usize,
    conversions: Vec<Conversion>,
}

fn replay(
    dictionary: &Dictionary<'_>,
    rows: &[HistoryRow],
    learning: &[LearningRow],
) -> Result<Replay, String> {
    let mut result = Replay {
        history_rows: rows.len(),
        ..Replay::default()
    };
    let Some(first_day) = rows.iter().map(|row| row.day).min() else {
        return Ok(result);
    };
    result.history_days = rows
        .iter()
        .map(|row| row.day)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let before = ReplayLearning::new(false);
    let after = ReplayLearning::new(true);
    for record in learning.iter().filter(|record| record.day < first_day) {
        for replay in [&before, &after] {
            replay.set_day(record.day);
            if record.rejection {
                replay.seed_rejection(
                    &record.reading,
                    &record.surface,
                    record.left_context,
                    record.right_context,
                );
            } else {
                replay.seed(
                    &record.reading,
                    &record.surface,
                    record.left_context,
                    record.right_context,
                );
            }
        }
        if record.rejection {
            result.seeded_rejections += 1;
        } else {
            result.seeded += 1;
        }
    }

    let mut converter = Converter::new();
    let mut sessions = BTreeMap::<u64, SessionReplay>::new();
    let mut engine = None;
    for row in rows {
        before.set_day(row.day);
        after.set_day(row.day);
        let session = sessions.entry(row.session).or_default();
        if session.scope != row.scope {
            // Another field: nothing left of the caret is known any more.
            session.trail.clear();
            session.scope.clone_from(&row.scope);
        }
        let owner = row.session;
        match &row.event {
            Event::Engine(version) => {
                if version.is_some() {
                    engine = *version;
                }
            }
            Event::AiText => session.trail.clear(),
            Event::Key(key) => {
                replay_key(key, session, owner, engine, [&before, &after]);
            }
            Event::Commit(commit) => {
                result.commits += 1;
                let candidates = converter
                    .convert(
                        dictionary,
                        &commit.reading,
                        ConversionOptions {
                            initial_right_id: commit.left_context,
                            skip_input_repair: true,
                            ..ConversionOptions::default()
                        },
                    )
                    .map_err(|error| format!("conversion failed: {error:?}"))?;
                let pending = session
                    .pending
                    .take()
                    .filter(|pending| pending.reading == commit.reading);
                if let (Some(pending), false) = (pending, candidates.is_empty()) {
                    let choose = |replay: &ReplayLearning| {
                        let choice = replay.first_candidate(
                            &commit.reading,
                            commit.left_context,
                            candidates,
                        );
                        (
                            Choice {
                                surface: candidates[choice.index].text().to_owned(),
                                learned: choice.learned,
                            },
                            choice.exact,
                        )
                    };
                    let (before_choice, before_exact) = choose(&before);
                    let (after_choice, _) = choose(&after);
                    if before_exact
                        && pending
                            .engine
                            .is_some_and(|engine| engine >= FIDELITY_SINCE)
                    {
                        result.fidelity_conversions += 1;
                        if before_choice.surface == pending.first_candidate {
                            result.fidelity_matches += 1;
                        }
                    }
                    session.conversions.push(result.conversions.len());
                    result.conversions.push(Conversion {
                        reading: commit.reading.clone(),
                        committed: commit.surface.clone(),
                        final_choice: commit.surface.clone(),
                        before: before_choice,
                        after: after_choice,
                    });
                }
                let replayed = ReplayCommit {
                    reading: &commit.reading,
                    surface: &commit.surface,
                    left_context: commit.left_context,
                    right_context: commit.right_context,
                };
                let retype = session.trail.commit(replayed);
                if let Some(correction) = &retype.correction {
                    result.corrections += 1;
                    for rejected in correction.rejected() {
                        // The erased conversion's final choice is the retyped surface.
                        if let Some(&index) = session.conversions.iter().rev().find(|&&index| {
                            let conversion = &result.conversions[index];
                            conversion.reading == commit.reading && conversion.committed == rejected
                        }) {
                            result.conversions[index]
                                .final_choice
                                .clone_from(&commit.surface);
                        }
                    }
                } else if retype.retyped {
                    result.same_surface_retypes += 1;
                }
                let learnable = !session.transformed;
                before.stage_commit(owner, replayed, learnable, retype.correction.as_ref());
                after.stage_commit(owner, replayed, learnable, retype.correction.as_ref());
                session.last_commit = Some(commit.surface.clone());
            }
        }
    }
    Ok(result)
}

/// Drives one key row the way the dispatcher settles staged learning and
/// the correction trail.
fn replay_key(
    key: &KeyRow,
    session: &mut SessionReplay,
    owner: u64,
    engine: Option<(u32, u32, u32)>,
    replays: [&ReplayLearning; 2],
) {
    if key.action.starts_with("transform_") {
        session.transformed = true;
    }
    if matches!(key.state_before, STATE_COMPOSING | STATE_PREDICTING)
        && key.state_after == STATE_CONVERTING
    {
        // A conversion learns every surviving commit before ranking.
        for replay in replays {
            replay.confirm_all_staged();
        }
        session.pending = Some(PendingConversion {
            reading: key.preedit_before.clone(),
            first_candidate: key.preedit_after.clone(),
            engine,
        });
    } else if key.action == "undo_commit" && key.delete_before > 0 {
        // The history does not record the host's outcome; assume applied.
        match &session.last_commit {
            Some(surface) => session.trail.undo_applied(surface),
            None => session.trail.clear(),
        }
        for replay in replays {
            replay.discard_staged(owner);
        }
    } else if key.state_before == STATE_IDLE
        && key.state_after == STATE_IDLE
        && key.preedit_before.is_empty()
    {
        let erased = key.key_code == KEY_BACKSPACE && !key.consumed;
        if erased && key.modifiers & (MODIFIER_CTRL | MODIFIER_ALT) == 0 {
            session.trail.erase();
        } else {
            session.trail.clear();
        }
        for replay in replays {
            if erased {
                replay.discard_staged(owner);
            } else {
                replay.confirm_staged(owner);
            }
        }
    }
    if key.state_after == STATE_IDLE {
        session.pending = None;
        session.transformed = false;
    }
}

/// How the two replays compare on the conversions where they differ.
#[derive(Debug, Default, PartialEq, Eq)]
struct Verdict {
    differing: usize,
    gain: usize,
    harm: usize,
    neither: usize,
    /// Harm where correction learning fell back to the dictionary order.
    dictionary_order_harm: usize,
}

impl Replay {
    fn verdict(&self) -> Verdict {
        let mut verdict = Verdict::default();
        for conversion in &self.conversions {
            if conversion.before.surface == conversion.after.surface {
                continue;
            }
            verdict.differing += 1;
            if conversion.after.surface == conversion.final_choice {
                verdict.gain += 1;
            } else if conversion.before.surface == conversion.final_choice {
                verdict.harm += 1;
                if !conversion.after.learned && conversion.before.learned {
                    verdict.dictionary_order_harm += 1;
                }
            } else {
                verdict.neither += 1;
            }
        }
        verdict
    }

    fn fidelity_percent(&self) -> Option<u64> {
        (self.fidelity_conversions > 0).then(|| {
            u64::try_from(self.fidelity_matches * 100 / self.fidelity_conversions)
                .unwrap_or(u64::MAX)
        })
    }

    fn report(&self) -> String {
        let verdict = self.verdict();
        let fidelity = self.fidelity_percent();
        let gain_holds = verdict.gain >= 2 * (verdict.harm + verdict.dictionary_order_harm);
        let fidelity_holds = fidelity.is_some_and(|percent| percent >= RELEASE_FIDELITY_PERCENT);
        let mut output = String::new();
        let mut line = |name: &str, value: String| {
            output.push_str(name);
            output.push('\t');
            output.push_str(&value);
            output.push('\n');
        };
        line("history-rows", self.history_rows.to_string());
        line("history-days", self.history_days.to_string());
        line("seeded-records", self.seeded.to_string());
        line("seeded-rejections", self.seeded_rejections.to_string());
        line("commits", self.commits.to_string());
        line("conversions", self.conversions.len().to_string());
        line("corrections", self.corrections.to_string());
        line(
            "same-surface-retypes",
            self.same_surface_retypes.to_string(),
        );
        line("differing-conversions", verdict.differing.to_string());
        line("gain", verdict.gain.to_string());
        line("harm", verdict.harm.to_string());
        line("neither", verdict.neither.to_string());
        line(
            "dictionary-order-harm",
            verdict.dictionary_order_harm.to_string(),
        );
        line(
            "fidelity",
            format!(
                "{}/{} ({})",
                self.fidelity_matches,
                self.fidelity_conversions,
                fidelity.map_or_else(|| "n/a".to_owned(), |percent| format!("{percent}%"))
            ),
        );
        line(
            "gate",
            if gain_holds && fidelity_holds {
                "pass".to_owned()
            } else {
                format!(
                    "fail (gain rule {}, fidelity rule {})",
                    if gain_holds { "holds" } else { "fails" },
                    if fidelity_holds { "holds" } else { "fails" }
                )
            },
        );
        output
    }

    fn examples(&self) -> String {
        let mut output = String::from("reading\tfinal\tbefore\tafter\n");
        for conversion in &self.conversions {
            if conversion.before.surface != conversion.after.surface {
                output.push_str(&format!(
                    "{}\t{}\t{}\t{}\n",
                    conversion.reading,
                    conversion.final_choice,
                    conversion.before.surface,
                    conversion.after.surface
                ));
            }
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Right IDs: 1 仮名, 2 加奈 and 候補NN, 3 電気.
    fn dictionary_image() -> Vec<u8> {
        let mut source = String::from(
            "# license: MIT\nreading\tsurface\tleft_id\tright_id\tword_cost\tprediction_cost\tflags\tannotation\n\
             かな\t仮名\t0\t1\t100\t100\t\tfixture\n\
             かな\t加奈\t0\t2\t200\t200\t\tfixture\n\
             でんき\t電気\t0\t3\t100\t100\t\tfixture\n",
        );
        for index in 3..=6 {
            let cost = 200 + index * 10;
            source.push_str(&format!(
                "かな\t候補{index:02}\t0\t2\t{cost}\t{cost}\t\tfixture\n"
            ));
        }
        let entries = dictc_core::parse_entries("conversion.tsv", &source).expect("entries");
        let matrix = dictc_core::parse_connection(
            "matrix.tsv",
            "# license: MIT\nclasses\t4\ndefault\t0\n",
            false,
        )
        .expect("matrix");
        dictc_core::compile(&entries, &matrix).expect("dictionary image")
    }

    const DAY: u64 = 20_000;

    struct History {
        sequence: u64,
        text: String,
    }

    impl History {
        fn new() -> Self {
            Self {
                sequence: 0,
                text: String::from("# sakura-input-history-format: 1\nkind\tsequence\n"),
            }
        }

        fn row(&mut self, kind: &str, session: u64, fill: impl FnOnce(&mut Vec<String>)) {
            self.sequence += 1;
            let mut fields = vec![String::new(); HISTORY_COLUMNS];
            fields[0] = kind.to_owned();
            fields[1] = self.sequence.to_string();
            fields[2] = (DAY * DAY_MS + self.sequence).to_string();
            fields[3] = session.to_string();
            fields[4] = "normal".to_owned();
            fill(&mut fields);
            self.text.push_str(&fields.join("\t"));
            self.text.push('\n');
        }

        fn engine(&mut self, version: &str) {
            self.row("engine", 0, |fields| fields[38] = version.to_owned());
        }

        #[allow(clippy::too_many_arguments)]
        fn key(
            &mut self,
            session: u64,
            key_code: u16,
            modifiers: u8,
            states: (u8, u8),
            preedit: (&str, &str),
            delete_before: u16,
            action: &str,
        ) {
            self.row("key", session, |fields| {
                fields[5] = key_code.to_string();
                fields[7] = modifiers.to_string();
                fields[8] = "false".to_owned();
                fields[9] = "false".to_owned();
                fields[10] = states.0.to_string();
                fields[11] = states.1.to_string();
                fields[14] = preedit.0.to_owned();
                fields[15] = preedit.1.to_owned();
                fields[17] = delete_before.to_string();
                fields[18] = "false".to_owned();
                fields[19] = action.to_owned();
                fields[20] = "0".to_owned();
            });
        }

        /// Space on a composition, showing `first` as the first candidate.
        fn convert(&mut self, session: u64, reading: &str, first: &str) {
            self.key(
                session,
                2,
                0,
                (STATE_COMPOSING, STATE_CONVERTING),
                (reading, first),
                0,
                "convert",
            );
        }

        /// The commit record, then the Enter that produced it.
        fn commit(&mut self, session: u64, reading: &str, surface: &str, left: u16, right: u16) {
            self.row("commit", session, |fields| {
                fields[21] = reading.to_owned();
                fields[22] = surface.to_owned();
                fields[23] = left.to_string();
                fields[24] = right.to_string();
            });
            self.key(
                session,
                3,
                0,
                (STATE_CONVERTING, STATE_IDLE),
                (surface, ""),
                0,
                "commit",
            );
        }

        fn backspace(&mut self, session: u64) {
            self.key(
                session,
                KEY_BACKSPACE,
                0,
                (STATE_IDLE, STATE_IDLE),
                ("", ""),
                0,
                "unbound",
            );
        }

        fn arrow(&mut self, session: u64) {
            self.key(
                session,
                8,
                0,
                (STATE_IDLE, STATE_IDLE),
                ("", ""),
                0,
                "unbound",
            );
        }
    }

    /// 仮名 learned eight times after 電気, before the history starts.
    fn seeded_learning() -> String {
        let mut text = String::from(
            "# sakura-learning-format: 3\nsequence\tday\tleft-context\tright-context\treading\tsurface\n",
        );
        for sequence in 1..=8 {
            text.push_str(&format!("{sequence}\t{}\t3\t1\tかな\t仮名\n", DAY - 1));
        }
        // Learned during the history window: never seeded.
        text.push_str(&format!("9\t{DAY}\t3\t2\tかな\t加奈\n"));
        text
    }

    fn run(history: &History, learning: &str) -> Replay {
        let image = dictionary_image();
        let dictionary = Dictionary::parse(&image).expect("dictionary");
        let rows = parse_history(&history.text).expect("history");
        let records = parse_learning(learning).expect("learning");
        replay(&dictionary, &rows, &records).expect("replay")
    }

    /// 電気 then かな→仮名 in `session`, as the user typed it.
    fn denki_then_kana(history: &mut History, session: u64, first: &str, surface: &str) {
        history.convert(session, "でんき", "電気");
        history.commit(session, "でんき", "電気", 0, 3);
        history.convert(session, "かな", first);
        let right = if surface == "仮名" { 1 } else { 2 };
        history.commit(session, "かな", surface, 3, right);
    }

    #[test]
    fn a_correction_is_counted_and_turns_the_next_conversion_into_a_gain() {
        let mut history = History::new();
        history.engine("2.0.15");
        denki_then_kana(&mut history, 1, "仮名", "仮名");
        history.backspace(1);
        history.backspace(1);
        history.convert(1, "かな", "仮名");
        // Retyped after the erase: the carried left context is gone (#273).
        history.commit(1, "かな", "加奈", 0, 2);
        history.arrow(1);
        // Later, in another field: the user again ends with 加奈.
        denki_then_kana(&mut history, 2, "仮名", "加奈");

        let replay = run(&history, &seeded_learning());
        assert_eq!(replay.seeded, 8);
        assert_eq!(replay.commits, 5);
        assert_eq!(replay.conversions.len(), 5);
        assert_eq!(replay.corrections, 1);
        assert_eq!(replay.same_surface_retypes, 0);
        assert_eq!(
            replay.verdict(),
            Verdict {
                differing: 1,
                gain: 1,
                harm: 0,
                neither: 0,
                dictionary_order_harm: 0,
            }
        );
        // The erased conversion's final choice became the retyped surface.
        let erased = replay
            .conversions
            .iter()
            .find(|conversion| conversion.committed == "仮名")
            .expect("erased conversion");
        assert_eq!(erased.final_choice, "加奈");
        // Exact learning picked 仮名 after 電気 twice and, in the second
        // session, 電気 itself; the history showed the same first candidates.
        assert_eq!(replay.fidelity_conversions, 3);
        assert_eq!(replay.fidelity_matches, 3);
        assert!(
            replay.report().contains("gate\tpass\n"),
            "{}",
            replay.report()
        );
    }

    #[test]
    fn a_same_surface_retype_is_counted_without_a_correction() {
        let mut history = History::new();
        history.engine("2.0.15");
        denki_then_kana(&mut history, 1, "仮名", "仮名");
        history.backspace(1);
        history.backspace(1);
        history.convert(1, "かな", "仮名");
        history.commit(1, "かな", "仮名", 0, 1);

        let replay = run(&history, &seeded_learning());
        assert_eq!(replay.corrections, 0);
        assert_eq!(replay.same_surface_retypes, 1);
        assert_eq!(replay.verdict().differing, 0);
    }

    #[test]
    fn an_interrupted_erase_is_not_a_correction() {
        let mut history = History::new();
        history.engine("2.0.15");
        denki_then_kana(&mut history, 1, "仮名", "仮名");
        history.backspace(1);
        history.arrow(1);
        history.backspace(1);
        history.convert(1, "かな", "仮名");
        history.commit(1, "かな", "加奈", 0, 2);

        let replay = run(&history, &seeded_learning());
        assert_eq!(replay.corrections, 0);
        assert_eq!(replay.same_surface_retypes, 0);
    }

    #[test]
    fn a_later_kept_choice_of_the_erased_surface_counts_as_harm() {
        let mut history = History::new();
        history.engine("2.0.15");
        denki_then_kana(&mut history, 1, "仮名", "仮名");
        history.backspace(1);
        history.backspace(1);
        history.convert(1, "かな", "仮名");
        history.commit(1, "かな", "加奈", 0, 2);
        history.arrow(1);
        // Later the user keeps 仮名 after 電気 after all.
        denki_then_kana(&mut history, 2, "仮名", "仮名");

        let replay = run(&history, &seeded_learning());
        let verdict = replay.verdict();
        assert_eq!((verdict.gain, verdict.harm), (0, 1));
        assert!(replay.report().contains("gate\tfail (gain rule fails"));
    }

    #[test]
    fn fidelity_reads_only_exact_learning_on_recent_engines() {
        let mut history = History::new();
        history.engine("2.0.12");
        denki_then_kana(&mut history, 1, "候補03", "仮名");
        history.engine("2.0.13");
        denki_then_kana(&mut history, 2, "候補03", "仮名");

        let replay = run(&history, &seeded_learning());
        // 2.0.12 is too old to count. On 2.0.13 the learned 電気 agrees with
        // the history and the learned 仮名 does not.
        assert_eq!(replay.fidelity_conversions, 2);
        assert_eq!(replay.fidelity_matches, 1);
        assert!(replay.report().contains("fidelity\t1/2 (50%)\n"));
    }

    #[test]
    fn a_kana_correction_that_falls_back_to_the_dictionary_order_is_counted() {
        let mut learning = String::from(
            "# sakura-learning-format: 3\nsequence\tday\tleft-context\tright-context\treading\tsurface\n",
        );
        for sequence in 1..=8 {
            learning.push_str(&format!("{sequence}\t{}\t3\t2\tかな\t加奈\n", DAY - 1));
        }
        let mut history = History::new();
        history.engine("2.0.15");
        denki_then_kana(&mut history, 1, "加奈", "加奈");
        history.backspace(1);
        history.backspace(1);
        history.convert(1, "かな", "加奈");
        history.key(
            1,
            38,
            0,
            (STATE_CONVERTING, STATE_CONVERTING),
            ("加奈", "カナ"),
            0,
            "transform_katakana",
        );
        history.commit(1, "かな", "カナ", 0, 0);
        history.arrow(1);
        // Later the user keeps 加奈 after 電気 after all.
        denki_then_kana(&mut history, 2, "加奈", "加奈");

        let replay = run(&history, &learning);
        assert_eq!(replay.corrections, 1);
        let verdict = replay.verdict();
        assert_eq!(verdict.differing, 1);
        assert_eq!(verdict.harm, 1);
        assert_eq!(verdict.dictionary_order_harm, 1);
        let later = replay.conversions.last().expect("later conversion");
        assert_eq!(later.before.surface, "加奈");
        assert_eq!(later.after.surface, "仮名");
        assert!(!later.after.learned);
    }

    #[test]
    fn history_fields_are_unescaped_and_rows_need_every_column() {
        assert_eq!(unescape(r"a\tb\\c\nd"), "a\tb\\c\nd");
        assert!(parse_history("key\t1\t2\n").is_err());
        assert_eq!(parse_version("2.0.13"), Some((2, 0, 13)));
        assert_eq!(parse_version("2.0"), None);
        let learning = parse_learning(
            "# sakura-learning-format: 3\nsequence\tday\tleft-context\tright-context\treading\tsurface\n1\t2\t7\t9\tかな\t仮\\t名\n#reject\t2\t2\t7\t9\tかな\t加奈\n",
        )
        .expect("learning");
        assert_eq!(learning.len(), 2);
        assert_eq!(learning[0].surface, "仮\t名");
        assert!(learning[1].rejection);
    }
}
