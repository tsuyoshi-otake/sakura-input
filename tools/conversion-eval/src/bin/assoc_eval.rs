//! Applies the #474 association table to the shipping engine's unchanged
//! candidate lists — outside the engine, as a prototype measurement.
//!
//! The engine is never asked for a different ranking: the tool takes the
//! exact candidate list the accuracy gate measures, adds the table's
//! group-relative weights for the committed-context tail (and, switchably,
//! the other words inside the same candidate), and swaps the top candidate
//! only under the Issue's rules: same reading segmentation as the original
//! top, a margin over the original price, system-dictionary-only provenance
//! and inside the top N. Nothing about the product's conversion order
//! changes — this binary is a measurement harness.
//!
//! Subcommands:
//!
//! ```text
//! assoc-eval baseline --dictionary <dic> [--corpus-dir <dir>]
//!     Re-measure every corpus row with the association OFF and compare the
//!     tool's top candidates with the committed accuracy-gate snapshots:
//!     the 1,630 conversion rows and the 210 context rows must differ 0
//!     times (proves the same engine is measured).
//! assoc-eval grid --dictionary <dic> --table-dir <dir> --history <export>
//!         [--corpus-dir <dir>]
//!     Runs every v2 grid point: W picks counts-W{W}.tsv; each point prints
//!     context strict and the even-session tuning history's fixed/broken.
//!     Candidates are converted once per row and reused across all 216
//!     points (#474 v2 step 5).
//! assoc-eval run --dictionary <dic> --table <file> --corpus <file>
//!         [--config W,beta,lambda_milli,cap,margin,N,intra] [--per-row]
//!     Applies one frozen configuration to a separate context-format file —
//!     the verifier's held-out pairs (#474 condition 7).
//! assoc-eval replay --dictionary <dic> --table <file> --history <export>
//!         [--config ...] [--subset all|even|odd]
//!     Replays a developer-history export once under the frozen config,
//!     counts only (target, matches before/after, fixed, broken; strict
//!     homophone-swap and <=4-char-reading breakdowns). `--subset` selects
//!     sessions by numeric parity (#474 v2 step 4).
//! assoc-eval perf --dictionary <dic> --table <file>
//!     p50/p99 apply time per conversion and table-load memory — explicit
//!     outside-engine estimates.
//! ```
//!
//! Config strings are `W,beta,lambda_milli,cap,margin,top_n,intra` where
//! `beta` is the v2 smoothing strength in articles, `lambda_milli` is
//! λ × 1000 and `intra` is 0/1 for clue words inside the candidate itself.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

use sakura_core::dictionary::Dictionary;
use sakura_core::{ConversionInput, ConversionOptions, Converter};
use sakura_engine::committed_context::CommittedContext;
use sakura_engine::dictionary::ConversionService;
use sha2::{Digest, Sha256};

#[allow(dead_code)] // prediction rows and helpers are used by accuracy-gate only
#[path = "accuracy_gate/corpus.rs"]
mod corpus;
#[allow(dead_code)] // most measurement helpers are used by accuracy-gate only
#[path = "accuracy_gate/measure.rs"]
mod measure;
#[path = "../assoc_vocab.rs"]
mod vocab;

use corpus::{parse_context, parse_conversion, ContextRow};
use measure::{commit_carry, with_service};
use vocab::Vocab;

/// One frozen application configuration (#474 v2). The v2 counts files
/// store raw counts, so `beta` (the smoothing strength, in articles) is an
/// application-side parameter applied when the weight is computed.
#[derive(Debug, Clone, Copy)]
struct Config {
    /// Clue window in chars — selects the W-suffixed counts file.
    #[allow(dead_code)] // read only through file naming, kept for provenance
    window: usize,
    /// β smoothing strength in articles (#474 v2).
    beta: u64,
    /// λ × 1000 applied to the summed clue weight.
    lambda_milli: i64,
    /// Cap on the summed clue weight per candidate.
    cap: i64,
    /// Adjusted price must beat the original top's by at least this.
    margin: i64,
    /// Swappable candidates sit within the top N.
    top_n: usize,
    /// Also use the other words inside the candidate as clues (v2 grid
    /// keeps this off; the flag stays for ad-hoc `run`/`replay` configs).
    intra_candidate: bool,
}

impl Config {
    fn parse(text: &str) -> Result<Self, String> {
        let fields: Vec<&str> = text.split(',').collect();
        if fields.len() != 7 {
            return Err(format!(
                "config '{text}' needs 7 fields: W,beta,lambda_milli,cap,margin,N,intra"
            ));
        }
        let parse_i64 = |index: usize| {
            fields[index].parse().map_err(|_| {
                format!(
                    "config field {} ('{}') is not an integer",
                    index, fields[index]
                )
            })
        };
        Ok(Self {
            window: parse_i64(0)? as usize,
            beta: parse_i64(1)? as u64,
            lambda_milli: parse_i64(2)?,
            cap: parse_i64(3)?,
            margin: parse_i64(4)?,
            top_n: parse_i64(5)? as usize,
            intra_candidate: parse_i64(6)? != 0,
        })
    }
}

/// Everything the association check needs from one engine candidate.
#[derive(Debug, Clone)]
struct CandSnap {
    text: String,
    /// (reading_start, reading_end, text_start, text_end, left_id, right_id).
    segments: Vec<(u16, u16, u16, u16, u16, u16)>,
    /// The candidate came only from the system dictionary and is not a
    /// generated/repair/literal product.
    system_only: bool,
    /// Local cost, needed to reproduce the reranker feature inputs (cond. 11).
    cost: i64,
    /// `CandidateEvidenceClass::neural_group` for the gated pick (cond. 11).
    neural_group: Option<u8>,
}

fn snap(candidate: &sakura_core::ConversionCandidate) -> CandSnap {
    CandSnap {
        text: candidate.text().to_owned(),
        segments: candidate
            .segments()
            .iter()
            .map(|segment| {
                (
                    segment.reading_start,
                    segment.reading_end,
                    segment.text_start,
                    segment.text_end,
                    segment.left_id,
                    segment.right_id,
                )
            })
            .collect(),
        system_only: candidate.path_evidence().is_system_only()
            && candidate.origin() == sakura_core::conversion::CandidateOrigin::Direct
            && !candidate.is_synthetic_exact(),
        cost: candidate.cost,
        neural_group: candidate.evidence_class().neural_group(),
    }
}

/// One counted row of a v2 counts file: the raw article counts behind the
/// beta-smoothed weight (kept verbatim so β can change without recounting).
#[derive(Debug, Clone, Copy)]
struct CountRow {
    /// Articles where the clue co-occurs with this target inside the window.
    near: u64,
    /// `near` summed over every target sharing the reading.
    group_near: u64,
    /// `target_total / group_total` — the document-frequency share.
    share: f64,
}

/// The association counts for one window: (target entry, clue entry) ->
/// CountRow, plus for application the surface->entry lookup comes from
/// Vocab. Weights are computed on demand at the grid point's β.
struct Table {
    counts: HashMap<(u32, u32), CountRow>,
    pairs: usize,
    bytes: usize,
}

impl Table {
    /// w(c,t) in milli-units at smoothing `beta`:
    /// `500 * ln( ((near + β·share) / (group_near + β)) / share )`.
    /// A clue that appears once or twice lands on the share prior and earns
    /// ~0; only repeated co-occurrence earns weight (#474 v2 fix for the
    /// constant-weight tie the v1 reviewer flagged).
    fn weight(&self, target: u32, clue: u32, beta: u64) -> i64 {
        let Some(row) = self.counts.get(&(target, clue)) else {
            return 0;
        };
        if row.share <= 0.0 || row.group_near == 0 {
            return 0;
        }
        let beta = beta as f64;
        let p_hat = (row.near as f64 + beta * row.share) / (row.group_near as f64 + beta);
        (500.0 * (p_hat / row.share).ln()).round() as i64
    }
}

fn load_table(vocab: &Vocab, path: &Path) -> Result<Table, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("counts read: {e}"))?;
    let mut counts = HashMap::new();
    let mut pairs = 0usize;
    for (number, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 7 {
            return Err(format!(
                "counts:{}: expected 7 columns (reading/target/clue/near/group_near/target_total/group_total)",
                number + 1
            ));
        }
        let (reading, target, clue) = (fields[0], fields[1], fields[2]);
        let parse_u64 = |index: usize| {
            fields[index]
                .parse::<u64>()
                .map_err(|_| format!("counts:{}: field {} not an integer", number + 1, index))
        };
        let (near, group_near, target_total, group_total) =
            (parse_u64(3)?, parse_u64(4)?, parse_u64(5)?, parse_u64(6)?);
        if target_total == 0 || group_total == 0 {
            continue;
        }
        // The target resolves to the entry under (reading, surface); several
        // entries may share a surface so filter by reading.
        let target_entry = vocab
            .lookup(target)
            .iter()
            .find(|&&index| {
                vocab.entries[index as usize].reading == reading
                    && vocab.entries[index as usize].is_target
            })
            .copied();
        let clue_entry = vocab
            .lookup(clue)
            .iter()
            .find(|&&index| vocab.entries[index as usize].is_clue)
            .copied();
        let (Some(target_entry), Some(clue_entry)) = (target_entry, clue_entry) else {
            continue;
        };
        counts.insert(
            (target_entry, clue_entry),
            CountRow {
                near,
                group_near,
                share: target_total as f64 / group_total as f64,
            },
        );
        pairs += 1;
    }
    Ok(Table {
        counts,
        pairs,
        bytes: text.len(),
    })
}

/// Clue entry ids in a text: longest-match segmentation then the clue flag
/// (the single-kanji neighbour rule is part of `segment`-aware counting;
/// here a 1-char kanji token counts when it is not fused, matching the
/// counting rule closely enough for a tail of committed text).
fn clues_of(vocab: &Vocab, text: &str) -> Vec<u32> {
    vocab
        .segment(text)
        .into_iter()
        .filter_map(|token| token.entry)
        .filter_map(|index| vocab.clue_entry(index))
        .collect()
}

/// The summed table weight one candidate earns from `clues`. Target words
/// inside the candidate are its dictionary segments that resolve to target
/// entries — looked up per segment surface.
fn score_candidate(
    vocab: &Vocab,
    table: &Table,
    beta: u64,
    reading: &str,
    candidate: &CandSnap,
    clues: &[u32],
) -> i64 {
    let mut total = 0i64;
    for &(reading_start, reading_end, text_start, text_end, _, _) in &candidate.segments {
        let Some(segment_reading) = reading.get(reading_start as usize..reading_end as usize)
        else {
            continue;
        };
        let Some(surface) = candidate.text.get(text_start as usize..text_end as usize) else {
            continue;
        };
        let Some(&target) = vocab.lookup(surface).iter().find(|&&index| {
            vocab.entries[index as usize].is_target
                && vocab.entries[index as usize].reading == segment_reading
        }) else {
            continue;
        };
        for &clue in clues {
            total += table.weight(target, clue, beta);
        }
    }
    total
}

/// The dictionary price of a candidate, recomputed outside the engine:
/// sum of segment word costs plus the connection cost between consecutive
/// segments. `None` when any segment does not resolve to a single
/// dictionary entry (fused/generated pieces are not priceable this way).
fn price_of(
    vocab: &Vocab,
    dictionary: &Dictionary<'_>,
    reading: &str,
    candidate: &CandSnap,
) -> Option<i64> {
    let mut price = 0i64;
    let mut previous_right: Option<u16> = None;
    for &(reading_start, reading_end, text_start, text_end, left_id, right_id) in
        &candidate.segments
    {
        let surface = candidate.text.get(text_start as usize..text_end as usize)?;
        let segment_reading = reading.get(reading_start as usize..reading_end as usize)?;
        let entry = vocab.entry_for(segment_reading, surface, left_id, right_id)?;
        price += i64::from(entry.word_cost);
        if let Some(prev) = previous_right {
            price += i64::from(dictionary.connection_cost(prev, left_id)?);
        }
        previous_right = Some(right_id);
    }
    Some(price)
}

/// Whether `candidate` may replace `top` under the Issue's swap rule.
fn may_swap(
    stage: &Stage<'_>,
    reading: &str,
    table: &Table,
    clues: &[u32],
    top: &CandSnap,
    candidate: &CandSnap,
    config: &Config,
) -> bool {
    if !candidate.system_only || candidate.segments.len() != top.segments.len() {
        return false;
    }
    // Same reading segmentation as the original top.
    for (a, b) in candidate.segments.iter().zip(top.segments.iter()) {
        if a.0 != b.0 || a.1 != b.1 {
            return false;
        }
    }
    let (Some(price_top), Some(price_alt)) = (
        price_of(stage.vocab, stage.dictionary, reading, top),
        price_of(stage.vocab, stage.dictionary, reading, candidate),
    ) else {
        return false;
    };
    let score_top = score_candidate(stage.vocab, table, config.beta, reading, top, clues);
    let score_alt = score_candidate(stage.vocab, table, config.beta, reading, candidate, clues);
    // The association must actually improve the score; otherwise an empty
    // table would still rerank candidates by raw dictionary price.
    if score_alt <= score_top {
        return false;
    }
    let adjusted_top = price_top * 1000 - config.lambda_milli * score_top.min(config.cap);
    let adjusted_alt = price_alt * 1000 - config.lambda_milli * score_alt.min(config.cap);
    adjusted_alt <= adjusted_top - config.margin * 1000
}

/// The read-only world every evaluation path shares: the conversion
/// service, its dictionary for price lookups, and the vocabulary index.
struct Stage<'a> {
    service: &'a ConversionService,
    dictionary: &'a Dictionary<'a>,
    vocab: &'a Vocab,
}

/// Applies the association rule to `candidates`: returns the index of the
/// new top (0 = unchanged).
fn apply_association(
    stage: &Stage<'_>,
    reading: &str,
    table: &Table,
    clues: &[u32],
    candidates: &[CandSnap],
    config: &Config,
) -> usize {
    let Some(top) = candidates.first() else {
        return 0;
    };
    let mut best = 0usize;
    let mut best_adjusted = i64::MAX;
    for (index, candidate) in candidates.iter().enumerate().take(config.top_n).skip(1) {
        if may_swap(stage, reading, table, clues, top, candidate, config) {
            let score = score_candidate(stage.vocab, table, config.beta, reading, candidate, clues);
            let Some(price) = price_of(stage.vocab, stage.dictionary, reading, candidate) else {
                continue;
            };
            let adjusted = price * 1000 - config.lambda_milli * score.min(config.cap);
            if adjusted < best_adjusted {
                best_adjusted = adjusted;
                best = index;
            }
        }
    }
    best
}

/// Clues for a candidate when intra-candidate words are enabled: the clue
/// entries behind the candidate's *other* segments. The row's own context
/// clues are always included by the caller.
fn intra_clues(vocab: &Vocab, reading: &str, candidate: &CandSnap, skip: usize) -> Vec<u32> {
    let mut out = Vec::new();
    for (at, &(reading_start, reading_end, text_start, text_end, _, _)) in
        candidate.segments.iter().enumerate()
    {
        if at == skip {
            continue;
        }
        let Some(surface) = candidate.text.get(text_start as usize..text_end as usize) else {
            continue;
        };
        let _ = reading.get(reading_start as usize..reading_end as usize);
        out.extend(
            vocab
                .lookup(surface)
                .iter()
                .copied()
                .filter(|&index| vocab.entries[index as usize].is_clue),
        );
    }
    out
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|at| args.get(at + 1).cloned())
}

/// The flags each mode accepts, split into value-taking flags and boolean
/// flags. Everything else — unknown flags, positional strays, a flag left
/// dangling at the end of the line — stops the tool with usage (#474
/// review: a stray path after `--per-row` used to be silently ignored).
fn check_args(mode: &str, args: &[String]) {
    let (value_flags, bool_flags): (&[&str], &[&str]) = match mode {
        "baseline" => (&["--dictionary", "--corpus-dir"], &[]),
        "grid" => (
            &["--dictionary", "--table-dir", "--corpus-dir", "--history"],
            &[],
        ),
        "run" => (
            &["--dictionary", "--table", "--corpus", "--config"],
            &["--per-row"],
        ),
        "replay" => (
            &[
                "--dictionary",
                "--table",
                "--history",
                "--config",
                "--subset",
            ],
            &[],
        ),
        "perf" => (&["--dictionary", "--table", "--corpus-dir"], &[]),
        "dump" => (&["--dictionary", "--corpus", "--out"], &[]),
        _ => usage(),
    };
    let mut at = 2;
    while at < args.len() {
        let arg = args[at].as_str();
        if bool_flags.contains(&arg) {
            at += 1;
            continue;
        }
        if !value_flags.contains(&arg) {
            eprintln!("assoc-eval: unknown argument '{arg}'");
            usage();
        }
        match args.get(at + 1) {
            Some(value) if !value.starts_with("--") => at += 2,
            _ => {
                eprintln!("assoc-eval: {arg} needs a value");
                usage();
            }
        }
    }
}

fn usage() -> ! {
    eprintln!(
        "usage: assoc-eval <baseline|grid|run|replay|perf|dump> --dictionary <system.dic> [options]\n  \
         baseline: [--corpus-dir corpus/accuracy]\n  \
         grid:     --table-dir <dir> --history <export.tsv> [--corpus-dir corpus/accuracy]\n  \
         dump:     --corpus <context.tsv> --out <candidates.jsonl>\n  \
         run:      --table <file> --corpus <file> [--config W,beta,lam,cap,margin,N,intra] [--per-row]\n  \
         replay:   --table <file> --history <export.tsv> [--config ...] [--subset all|even|odd]\n  \
         perf:     --table <file> [--corpus-dir corpus/accuracy]"
    );
    std::process::exit(2);
}

/// The conversion candidates a row would see, extracted through the same
/// engine calls the accuracy gate uses.
fn context_candidates(
    service: &ConversionService,
    row: &ContextRow,
) -> Result<Vec<CandSnap>, String> {
    let (initial_right_id, bridge) = commit_carry(service, &row.clue_reading, &row.clue_surface)
        .map_err(|error| format!("{}: {error}", row.id))?;
    with_service(|| {
        service.with_conversion_input_bridge_hints(
            ConversionInput::ordinary(&row.reading),
            ConversionOptions {
                initial_right_id,
                ..ConversionOptions::default()
            },
            &[],
            bridge,
            |candidates, _| candidates.iter().map(snap).collect::<Vec<_>>(),
        )
    })
    .map_err(|error| format!("{}: context conversion failed: {error:?}", row.id))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(mode) = args.get(1).map(String::as_str) else {
        usage();
    };
    check_args(mode, &args);
    let dictionary_path = arg_value(&args, "--dictionary").unwrap_or_else(|| usage());
    let bytes = fs::read(&dictionary_path).expect("dictionary read");
    // Report the dictionary actually opened — never a transcribed hash
    // (#474: a copied SHA once named the wrong build in a baseline report).
    let dictionary_sha: String = Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    println!("dictionary: {dictionary_path} sha256 {dictionary_sha}");
    let service = ConversionService::from_static_bytes(Box::leak(bytes.into_boxed_slice()))
        .expect("conversion service");
    let dictionary = service.dictionary();
    let vocab = Vocab::load(&dictionary).expect("vocab");
    let corpus_dir =
        PathBuf::from(arg_value(&args, "--corpus-dir").unwrap_or_else(|| "corpus/accuracy".into()));

    match mode {
        "baseline" => baseline(&service, &vocab, &corpus_dir),
        "grid" => {
            let table_dir =
                PathBuf::from(arg_value(&args, "--table-dir").unwrap_or_else(|| usage()));
            let history = PathBuf::from(arg_value(&args, "--history").unwrap_or_else(|| usage()));
            grid(
                &service,
                &dictionary,
                &vocab,
                &corpus_dir,
                &table_dir,
                &history,
            );
        }
        "run" => {
            let table = PathBuf::from(arg_value(&args, "--table").unwrap_or_else(|| usage()));
            let corpus_path =
                PathBuf::from(arg_value(&args, "--corpus").unwrap_or_else(|| usage()));
            let config = arg_value(&args, "--config")
                .map(|s| Config::parse(&s).expect("config"))
                .unwrap_or(Config {
                    window: 0,
                    beta: 100,
                    lambda_milli: 1000,
                    cap: i64::MAX,
                    margin: 0,
                    top_n: 5,
                    intra_candidate: false,
                });
            run(
                &service,
                &dictionary,
                &vocab,
                &table,
                &corpus_path,
                &config,
                args.iter().any(|arg| arg == "--per-row"),
            );
        }
        "replay" => {
            let table = PathBuf::from(arg_value(&args, "--table").unwrap_or_else(|| usage()));
            let history = PathBuf::from(arg_value(&args, "--history").unwrap_or_else(|| usage()));
            let config = arg_value(&args, "--config")
                .map(|s| Config::parse(&s).expect("config"))
                .unwrap_or(Config {
                    window: 0,
                    beta: 100,
                    lambda_milli: 1000,
                    cap: i64::MAX,
                    margin: 0,
                    top_n: 5,
                    intra_candidate: false,
                });
            let subset = arg_value(&args, "--subset").unwrap_or_else(|| "all".into());
            if !matches!(subset.as_str(), "all" | "even" | "odd") {
                eprintln!("assoc-eval: --subset must be all|even|odd");
                usage();
            }
            replay(
                &service,
                &dictionary,
                &vocab,
                &table,
                &history,
                &config,
                &subset,
            );
        }
        "perf" => {
            let table = PathBuf::from(arg_value(&args, "--table").unwrap_or_else(|| usage()));
            perf(&service, &dictionary, &vocab, &corpus_dir, &table);
        }
        "dump" => {
            let corpus_path =
                PathBuf::from(arg_value(&args, "--corpus").unwrap_or_else(|| usage()));
            let out = PathBuf::from(arg_value(&args, "--out").unwrap_or_else(|| usage()));
            dump(&service, &corpus_path, &out);
        }
        _ => usage(),
    }
}

/// Rows for the grid/replay: measured outcome of one context row under a
/// config. `state` mirrors the gate's expected/variant/miss scoring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Expected,
    Variant,
    Miss,
}

fn state_of(top: &str, expected: &str, variants: &[String]) -> State {
    if top == expected {
        State::Expected
    } else if variants.iter().any(|variant| variant == top) {
        State::Variant
    } else {
        State::Miss
    }
}

fn load_corpora(corpus_dir: &Path) -> (Vec<corpus::ConversionRow>, Vec<ContextRow>) {
    let conversion = parse_conversion(
        &fs::read_to_string(corpus_dir.join("conversion.tsv")).expect("conversion.tsv"),
    )
    .expect("conversion parse");
    let context =
        parse_context(&fs::read_to_string(corpus_dir.join("context.tsv")).expect("context.tsv"))
            .expect("context parse");
    (conversion, context)
}

/// Verify mode 3: association OFF, the tool's tops must equal the committed
/// snapshots row for row.
fn baseline(service: &ConversionService, vocab: &Vocab, corpus_dir: &Path) {
    let _ = vocab;
    let (conversion_rows, context_rows) = load_corpora(corpus_dir);
    let snapshot = fs::read_to_string(corpus_dir.join("conversion.snapshot.tsv"))
        .expect("conversion snapshot");
    let context_snapshot =
        fs::read_to_string(corpus_dir.join("context.snapshot.tsv")).expect("context snapshot");
    let expected_conv: HashMap<String, String> = snapshot
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            (fields.len() >= 3).then(|| (fields[0].to_owned(), fields[2].to_owned()))
        })
        .collect();
    let expected_ctx: HashMap<String, String> = context_snapshot
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            (fields.len() >= 5).then(|| (fields[0].to_owned(), fields[4].to_owned()))
        })
        .collect();
    let expected_oneshot: HashMap<String, String> = context_snapshot
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split('\t').collect();
            (fields.len() >= 3).then(|| (fields[0].to_owned(), fields[2].to_owned()))
        })
        .collect();
    let dictionary = service.dictionary();
    let mut converter = Converter::new();
    let mut diffs = 0usize;
    for row in &conversion_rows {
        let candidates = converter
            .convert(&dictionary, &row.reading, ConversionOptions::default())
            .expect("convert");
        let top = candidates
            .first()
            .map(|c| c.text().to_owned())
            .unwrap_or_default();
        if expected_conv.get(&row.id) != Some(&top) {
            eprintln!(
                "diff {}: tool top '{}' vs snapshot '{:?}'",
                row.id,
                top,
                expected_conv.get(&row.id)
            );
            diffs += 1;
        }
    }
    for row in &context_rows {
        let candidates = context_candidates(service, row).expect("context candidates");
        let top = candidates
            .first()
            .map(|c| c.text.clone())
            .unwrap_or_default();
        if expected_ctx.get(&row.id) != Some(&top) {
            eprintln!(
                "diff {}: tool top '{}' vs snapshot '{:?}'",
                row.id,
                top,
                expected_ctx.get(&row.id)
            );
            diffs += 1;
        }
        let oneshot = converter
            .convert(
                &dictionary,
                &row.oneshot_reading,
                ConversionOptions::default(),
            )
            .expect("convert");
        let oneshot_top = oneshot
            .first()
            .map(|c| c.text().to_owned())
            .unwrap_or_default();
        if expected_oneshot.get(&row.id) != Some(&oneshot_top) {
            eprintln!(
                "diff {}: oneshot top '{}' vs snapshot '{:?}'",
                row.id,
                oneshot_top,
                expected_oneshot.get(&row.id)
            );
            diffs += 1;
        }
    }
    println!(
        "baseline: {} conversion rows, {} context rows, {} diffs",
        conversion_rows.len(),
        context_rows.len(),
        diffs
    );
}

/// Evaluate the 1,630 conversion rows + 210 context rows under one config.
/// Association applies wherever clues exist; for conversion rows the tail
/// is empty so only intra-candidate clues (when enabled) can score.
fn evaluate(
    stage: &Stage<'_>,
    conversion_rows: &[corpus::ConversionRow],
    context_rows: &[ContextRow],
    table: &Table,
    config: &Config,
    baseline_ctx: &HashMap<String, usize>,
) -> (
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
) {
    let mut conv_fixed = 0usize;
    let mut conv_broken = 0usize;
    let mut converter = Converter::new();
    let dict = stage.service.dictionary();
    // Snapshot tops for the conversion corpus mark the association OFF row
    // result; compare each row's state before/after applying.
    for row in conversion_rows {
        let candidates = converter
            .convert(&dict, &row.reading, ConversionOptions::default())
            .expect("convert");
        let snaps: Vec<CandSnap> = candidates.iter().map(snap).collect();
        let top_before = snaps.first().map(|c| c.text.clone()).unwrap_or_default();
        let clues = if config.intra_candidate {
            let mut c = Vec::new();
            for skip in 0..snaps.first().map(|s| s.segments.len()).unwrap_or(0) {
                c.extend(intra_clues(
                    stage.vocab,
                    &row.reading,
                    snaps.first().unwrap(),
                    skip,
                ));
            }
            c
        } else {
            Vec::new()
        };
        let index = apply_association(stage, &row.reading, table, &clues, &snaps, config);
        let top_after = snaps.get(index).map(|c| c.text.clone()).unwrap_or_default();
        if top_after != top_before {
            let before = state_of(&top_before, &row.expected, &row.variants);
            let after = state_of(&top_after, &row.expected, &row.variants);
            match (before, after) {
                (State::Miss, State::Expected | State::Variant) => conv_fixed += 1,
                (State::Expected | State::Variant, State::Miss) => conv_broken += 1,
                (State::Expected, State::Variant) | (State::Variant, State::Expected) => {}
                _ => {}
            }
        }
    }

    let mut ctx_lenient = 0usize;
    let mut ctx_strict = 0usize;
    let mut oneshot_lenient = 0usize;
    let mut oneshot_strict = 0usize;
    let mut pair_strict: BTreeMap<String, usize> = BTreeMap::new();
    let _ = baseline_ctx;
    for row in context_rows {
        // One-shot side is association-free (no committed context exists).
        let oneshot = {
            let candidates = converter
                .convert(&dict, &row.oneshot_reading, ConversionOptions::default())
                .expect("convert");
            candidates
                .first()
                .map(|c| c.text().to_owned())
                .unwrap_or_default()
        };
        let oneshot_variants: Vec<String> = row
            .variants
            .iter()
            .map(|variant| format!("{}{}", row.clue_surface, variant))
            .collect();
        let oneshot_state = state_of(&oneshot, &row.oneshot_expected, &oneshot_variants);
        if oneshot_state != State::Miss {
            oneshot_lenient += 1;
        }
        if oneshot_state == State::Expected {
            oneshot_strict += 1;
        }

        let snaps = context_candidates(stage.service, row).expect("context candidates");
        let mut clues = clues_of(stage.vocab, &row.clue_surface);
        if config.intra_candidate {
            if let Some(top) = snaps.first() {
                for skip in 0..top.segments.len() {
                    clues.extend(intra_clues(stage.vocab, &row.reading, top, skip));
                }
            }
        }
        let index = apply_association(stage, &row.reading, table, &clues, &snaps, config);
        let top_after = snaps.get(index).map(|c| c.text.clone()).unwrap_or_default();
        let state = state_of(&top_after, &row.expected, &row.variants);
        if state != State::Miss {
            ctx_lenient += 1;
        }
        if state == State::Expected {
            ctx_strict += 1;
        }
        if state == State::Expected {
            *pair_strict.entry(row.pair.clone()).or_insert(0) += 1;
        } else {
            pair_strict.entry(row.pair.clone()).or_insert(0);
        }
    }
    let pairs_both = pair_strict.values().filter(|&&n| n == 2).count();
    let pairs_one = pair_strict.values().filter(|&&n| n == 1).count();
    let pairs_neither = pair_strict.values().filter(|&&n| n == 0).count();
    (
        ctx_lenient,
        ctx_strict,
        oneshot_lenient,
        oneshot_strict,
        pairs_both,
        pairs_one,
        pairs_neither,
        conv_fixed,
        conv_broken,
    )
}

/// One measured commit row collected from the history export (#474 v2):
/// everything the association application needs, resolved at collection
/// time so the engine conversion runs once per row for the whole grid.
struct HistoryRow {
    reading: String,
    committed: String,
    clue_text: String,
    clue_reading: String,
    clue_surface: String,
    short: bool,
}

/// Whether a session id belongs to `subset` ("all", "even", "odd").
/// Non-numeric sessions only belong to "all".
fn session_in_subset(session: &str, subset: &str) -> bool {
    match subset {
        "all" => true,
        want => session
            .parse::<u64>()
            .map(|n| (n % 2 == 1) == (want == "odd"))
            .unwrap_or(false),
    }
}

/// History rows for the measured subset (#474 v2 step 4: even session ids
/// tune, odd confirm). Mirrors `replay`'s row semantics exactly — a
/// `commit` with scope `normal` whose session's previous key action was
/// `convert` is a target; the clue is the per-session committed tail plus
/// the previous commit's (reading, surface). Returns (target, skipped,
/// short_target, rows); `target` and `short_target` count converted
/// commits without a usable clue too, matching replay's denominators.
fn collect_history(history_path: &Path, subset: &str) -> (usize, usize, usize, Vec<HistoryRow>) {
    let mut input = String::new();
    fs::File::open(history_path)
        .expect("history open")
        .read_to_string(&mut input)
        .expect("history read");
    let mut header_seen = false;
    let mut indexes = (0, 0, 0, 0, 0, 0);
    let mut last_action: HashMap<String, String> = HashMap::new();
    let mut tails: HashMap<String, CommittedContext> = HashMap::new();
    let mut last: HashMap<String, (String, String)> = HashMap::new();
    let mut target = 0usize;
    let mut short_target = 0usize;
    let mut skipped = 0usize;
    let mut rows = Vec::new();
    for line in input.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if !header_seen {
            let names: Vec<&str> = line.split('\t').collect();
            let find = |name: &str| {
                names
                    .iter()
                    .position(|column| *column == name)
                    .unwrap_or(usize::MAX)
            };
            indexes = (
                find("kind"),
                find("session"),
                find("scope"),
                find("action"),
                find("reading"),
                find("surface"),
            );
            header_seen = true;
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let width = [
            indexes.0, indexes.1, indexes.2, indexes.3, indexes.4, indexes.5,
        ]
        .iter()
        .max()
        .copied()
        .unwrap_or(0)
            + 1;
        if fields.len() < width {
            continue;
        }
        if !session_in_subset(fields[indexes.1], subset) {
            continue;
        }
        if fields[indexes.0] == "key" {
            last_action.insert(fields[indexes.1].to_owned(), fields[indexes.3].to_owned());
            continue;
        }
        if fields[indexes.0] != "commit" || fields[indexes.2] != "normal" {
            continue;
        }
        let session = fields[indexes.1].to_owned();
        let converted = last_action.get(&session).is_some_and(|a| a == "convert");
        let tail = tails.entry(session.clone()).or_default();
        let clue_text = tail.as_str().to_owned();
        tail.append(fields[indexes.5]);
        let clue = last.insert(
            session.clone(),
            (fields[indexes.4].to_owned(), fields[indexes.5].to_owned()),
        );
        if !converted {
            continue;
        }
        target += 1;
        let short = fields[indexes.4].chars().count() <= 4;
        if short {
            short_target += 1;
        }
        let Some((clue_reading, clue_surface)) = clue else {
            skipped += 1;
            continue;
        };
        rows.push(HistoryRow {
            reading: fields[indexes.4].to_owned(),
            committed: fields[indexes.5].to_owned(),
            clue_text,
            clue_reading,
            clue_surface,
            short,
        });
    }
    (target, skipped, short_target, rows)
}

/// A cached evaluation row: candidates the engine produced once, reused
/// for every grid point (#474 v2 step 5 — conversion is not repeated per
/// point, only the association application is).
struct CachedCtx {
    reading: String,
    expected: String,
    variants: Vec<String>,
    clues: Vec<u32>,
    snaps: Vec<CandSnap>,
}

struct CachedHist {
    reading: String,
    committed: String,
    clues: Vec<u32>,
    snaps: Vec<CandSnap>,
}

/// One grid point's parameters and measured outcomes (#474 v2 selection).
struct GridPoint {
    window: usize,
    beta: u64,
    lambda: i64,
    cap: i64,
    margin: i64,
    top_n: usize,
    ctx_strict: usize,
    fixed: usize,
    broken: usize,
}

/// The v2 grid (#474 step 5): W {30,60,120} x beta {10,100,1000} x lambda
/// {500,1000,2000} x cap {1500,3000} x margin {0,500} x N {3,8}, intra
/// always off — 216 points on cached candidates. `fixed`/`broken` are
/// measured on the even-session tuning half of `--history`; `ctx_strict`
/// on context.tsv. Selection implements step 6 verbatim.
fn grid(
    service: &ConversionService,
    dictionary: &Dictionary<'_>,
    vocab: &Vocab,
    corpus_dir: &Path,
    table_dir: &Path,
    history_path: &Path,
) {
    let (_, context_rows) = load_corpora(corpus_dir);
    let stage = Stage {
        service,
        dictionary,
        vocab,
    };
    let mut cached_ctx: Vec<CachedCtx> = Vec::with_capacity(context_rows.len());
    for row in &context_rows {
        let snaps = context_candidates(service, row).expect("context candidates");
        cached_ctx.push(CachedCtx {
            reading: row.reading.clone(),
            expected: row.expected.clone(),
            variants: row.variants.clone(),
            clues: clues_of(vocab, &row.clue_surface),
            snaps,
        });
    }
    eprintln!("grid: cached {} context rows", cached_ctx.len());
    let (hist_target, mut hist_skipped, _, history_rows) = collect_history(history_path, "even");
    let mut cached_hist: Vec<CachedHist> = Vec::with_capacity(history_rows.len());
    for row in &history_rows {
        let (initial_right_id, bridge) =
            commit_carry(service, &row.clue_reading, &row.clue_surface).unwrap_or_default();
        let snaps = with_service(|| {
            service.with_conversion_input_bridge_hints(
                ConversionInput::ordinary(&row.reading),
                ConversionOptions {
                    initial_right_id,
                    ..ConversionOptions::default()
                },
                &[],
                bridge,
                |candidates, _| candidates.iter().map(snap).collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default();
        if snaps.is_empty() {
            hist_skipped += 1;
            continue;
        }
        cached_hist.push(CachedHist {
            reading: row.reading.clone(),
            committed: row.committed.clone(),
            clues: clues_of(vocab, &row.clue_text),
            snaps,
        });
    }
    eprintln!(
        "grid: cached {}/{} even-session history rows ({} skipped)",
        cached_hist.len(),
        hist_target,
        hist_skipped
    );

    let windows = [30usize, 60, 120];
    let betas = [10u64, 100, 1000];
    let lambdas = [500i64, 1000, 2000];
    let caps = [1500i64, 3000];
    let margins = [0i64, 500];
    let top_ns = [3usize, 8];
    println!(
        "W\tbeta\tlambda\tcap\tmargin\tN\tintra\tctx_strict\thist_target\thist_fixed\thist_broken"
    );
    let mut points: Vec<GridPoint> = Vec::new();
    for &window in &windows {
        let table_path = table_dir.join(format!("counts-W{window}.tsv"));
        let table = match load_table(vocab, &table_path) {
            Ok(table) => table,
            Err(error) => {
                eprintln!("W={window}: {error} (skipped)");
                continue;
            }
        };
        for &beta in &betas {
            for &lambda in &lambdas {
                for &cap in &caps {
                    for &margin in &margins {
                        for &top_n in &top_ns {
                            let config = Config {
                                window,
                                beta,
                                lambda_milli: lambda,
                                cap,
                                margin,
                                top_n,
                                intra_candidate: false,
                            };
                            let mut ctx_strict = 0usize;
                            for row in &cached_ctx {
                                let index = apply_association(
                                    &stage,
                                    &row.reading,
                                    &table,
                                    &row.clues,
                                    &row.snaps,
                                    &config,
                                );
                                let top =
                                    row.snaps.get(index).map(|c| c.text.as_str()).unwrap_or("");
                                if state_of(top, &row.expected, &row.variants) == State::Expected {
                                    ctx_strict += 1;
                                }
                            }
                            let mut fixed = 0usize;
                            let mut broken = 0usize;
                            for row in &cached_hist {
                                let index = apply_association(
                                    &stage,
                                    &row.reading,
                                    &table,
                                    &row.clues,
                                    &row.snaps,
                                    &config,
                                );
                                let before = &row.snaps[0].text;
                                let after = &row.snaps[index].text;
                                if *before != row.committed && *after == row.committed {
                                    fixed += 1;
                                }
                                if *before == row.committed && *after != row.committed {
                                    broken += 1;
                                }
                            }
                            println!(
                                "{window}\t{beta}\t{lambda}\t{cap}\t{margin}\t{top_n}\t0\t{ctx_strict}\t{}\t{fixed}\t{broken}",
                                cached_hist.len()
                            );
                            points.push(GridPoint {
                                window,
                                beta,
                                lambda,
                                cap,
                                margin,
                                top_n,
                                ctx_strict,
                                fixed,
                                broken,
                            });
                        }
                    }
                }
            }
        }
    }
    // Selection (#474 step 6): keep fixed >= 5*broken and ctx_strict >= 105,
    // maximize fixed - broken, ties take the conservative order
    // margin>cap^lambda^beta>N^W^ (bigger margin first, then smaller cap,
    // smaller lambda, bigger beta, smaller N, smaller W).
    let mut best: Option<&GridPoint> = None;
    for point in &points {
        if !(point.fixed as i64 >= 5 * point.broken as i64 && point.ctx_strict >= 105) {
            continue;
        }
        let diff = point.fixed as i64 - point.broken as i64;
        let better = match best {
            None => true,
            Some(b) => {
                let b_diff = b.fixed as i64 - b.broken as i64;
                diff > b_diff
                    || (diff == b_diff
                        && (
                            point.margin,
                            -point.cap,
                            -point.lambda,
                            point.beta,
                            -(point.top_n as i64),
                            -(point.window as i64),
                        ) > (
                            b.margin,
                            -b.cap,
                            -b.lambda,
                            b.beta,
                            -(b.top_n as i64),
                            -(b.window as i64),
                        ))
            }
        };
        if better {
            best = Some(point);
        }
    }
    match best {
        Some(p) => println!(
            "selected: W={} beta={} lambda={} cap={} margin={} N={} \
             (ctx_strict={} fixed={} broken={} diff={})",
            p.window,
            p.beta,
            p.lambda,
            p.cap,
            p.margin,
            p.top_n,
            p.ctx_strict,
            p.fixed,
            p.broken,
            p.fixed as i64 - p.broken as i64
        ),
        None => println!("selected: no improvement"),
    }
}

/// Held-out file evaluation (#474 condition 7): a verifier supplies a file
/// in context.tsv format and this prints the same summary lines. `--per-row`
/// additionally prints one line per row with the before/after state, and the
/// summary gains fixed (miss→hit), broken (hit→miss) and unreachable
/// (expected absent from the candidate list, so no ranking could find it).
fn run(
    service: &ConversionService,
    dictionary: &Dictionary<'_>,
    vocab: &Vocab,
    table_path: &Path,
    corpus_path: &Path,
    config: &Config,
    per_row: bool,
) {
    let rows = parse_context(&fs::read_to_string(corpus_path).expect("held-out corpus"))
        .expect("held-out parse");
    let table = load_table(vocab, table_path).expect("table");
    let stage = Stage {
        service,
        dictionary,
        vocab,
    };
    let empty: Vec<corpus::ConversionRow> = Vec::new();
    let (
        ctx_lenient,
        ctx_strict,
        oneshot_lenient,
        oneshot_strict,
        pairs_both,
        pairs_one,
        pairs_neither,
        _,
        _,
    ) = evaluate(&stage, &empty, &rows, &table, config, &HashMap::new());
    let mut fixed = 0usize;
    let mut broken = 0usize;
    let mut unreachable = 0usize;
    if per_row {
        println!("id\tpair\tbefore\tafter\tverdict");
    }
    for row in &rows {
        let snaps = context_candidates(service, row).expect("context candidates");
        let reachable = snaps
            .iter()
            .any(|snap| state_of(&snap.text, &row.expected, &row.variants) != State::Miss);
        let mut clues = clues_of(vocab, &row.clue_surface);
        if config.intra_candidate {
            if let Some(top) = snaps.first() {
                for skip in 0..top.segments.len() {
                    clues.extend(intra_clues(vocab, &row.reading, top, skip));
                }
            }
        }
        let index = apply_association(&stage, &row.reading, &table, &clues, &snaps, config);
        let before = snaps
            .first()
            .map(|snap| state_of(&snap.text, &row.expected, &row.variants))
            .unwrap_or(State::Miss);
        let after = snaps
            .get(index)
            .map(|snap| state_of(&snap.text, &row.expected, &row.variants))
            .unwrap_or(State::Miss);
        if !reachable {
            unreachable += 1;
        }
        if before == State::Miss && after != State::Miss {
            fixed += 1;
        }
        if before != State::Miss && after == State::Miss {
            broken += 1;
        }
        if per_row {
            let verdict = if !reachable {
                "unreachable"
            } else if before == State::Miss && after != State::Miss {
                "fixed"
            } else if before != State::Miss && after == State::Miss {
                "broken"
            } else {
                "unchanged"
            };
            println!(
                "{}\t{}\t{:?}\t{:?}\t{verdict}",
                row.id, row.pair, before, after
            );
        }
    }
    println!("rows={}", rows.len());
    println!("context_lenient={ctx_lenient} context_strict={ctx_strict}");
    println!("oneshot_lenient={oneshot_lenient} oneshot_strict={oneshot_strict}");
    println!("pairs_both={pairs_both} pairs_one={pairs_one} pairs_neither={pairs_neither}");
    println!("fixed={fixed} broken={broken} unreachable={unreachable}");
}

/// #474 condition 11 input: dump each context row's engine candidates (top 6,
/// the reranker's TOP_K) with cost and neural group, as JSONL for the
/// neural-worker `context-probe` binary. Candidate order is unchanged.
fn dump(service: &ConversionService, corpus_path: &Path, out_path: &Path) {
    let rows =
        parse_context(&fs::read_to_string(corpus_path).expect("corpus")).expect("context parse");
    let mut out = String::new();
    let mut skipped = 0usize;
    for row in &rows {
        let snaps = context_candidates(service, row).expect("context candidates");
        if snaps.len() < 2 {
            skipped += 1;
        }
        let candidates: Vec<serde_json::Value> = snaps
            .iter()
            .take(6)
            .map(|candidate| {
                serde_json::json!({
                    "text": candidate.text,
                    "cost": candidate.cost,
                    "neural_group": candidate.neural_group,
                })
            })
            .collect();
        out.push_str(
            &serde_json::json!({
                "id": row.id,
                "pair": row.pair,
                "expected": row.expected,
                "variants": row.variants,
                "reading": row.reading,
                "oneshot_reading": row.oneshot_reading,
                "oneshot_expected": row.oneshot_expected,
                "clue_surface": row.clue_surface,
                "candidates": candidates,
            })
            .to_string(),
        );
        out.push('\n');
    }
    fs::write(out_path, &out).expect("dump write");
    eprintln!(
        "dump: {} rows ({} with <2 candidates), {} bytes",
        rows.len(),
        skipped,
        out.len()
    );
}

/// History replay under a frozen config (#474 condition 6): counts only.
/// Context for each measured commit is the per-session committed tail the
/// shared `CommittedContext` produces — the same 256-byte bound as the
/// engine.
fn replay(
    service: &ConversionService,
    dictionary: &Dictionary<'_>,
    vocab: &Vocab,
    table_path: &Path,
    history_path: &Path,
    config: &Config,
    subset: &str,
) {
    let table = load_table(vocab, table_path).expect("table");
    let stage = Stage {
        service,
        dictionary,
        vocab,
    };
    let (target, mut skipped, short_target, rows) = collect_history(history_path, subset);
    let mut before_match = 0usize;
    let mut after_match = 0usize;
    let mut fixed = 0usize;
    let mut broken = 0usize;
    // Same five counters restricted to rows whose BEFORE top is a strict
    // homophone swap (same okurigana shape, kanji differ).
    let mut swap_target = 0usize;
    let mut swap_after_match = 0usize;
    let mut swap_broken_new = 0usize;
    // Same five counters for readings of four characters or fewer.
    let mut short_before = 0usize;
    let mut short_after = 0usize;
    let mut short_fixed = 0usize;
    let mut short_broken = 0usize;
    for row in &rows {
        let short = row.short;
        // Same rule as the accuracy gate's replay: an unrecoverable previous
        // commit carries nothing, so the row is measured as a no-bridge
        // conversion rather than dropped.
        let (initial_right_id, bridge) =
            commit_carry(service, &row.clue_reading, &row.clue_surface).unwrap_or_default();
        let snaps = with_service(|| {
            service.with_conversion_input_bridge_hints(
                ConversionInput::ordinary(&row.reading),
                ConversionOptions {
                    initial_right_id,
                    ..ConversionOptions::default()
                },
                &[],
                bridge,
                |candidates, _| candidates.iter().map(snap).collect::<Vec<_>>(),
            )
        })
        .unwrap_or_default();
        if snaps.is_empty() {
            skipped += 1;
            continue;
        }
        let committed = row.committed.as_str();
        let before = snaps[0].text.clone();
        let mut clues = clues_of(vocab, &row.clue_text);
        if config.intra_candidate {
            for skip in 0..snaps[0].segments.len() {
                clues.extend(intra_clues(vocab, &row.reading, &snaps[0], skip));
            }
        }
        let index = apply_association(&stage, &row.reading, &table, &clues, &snaps, config);
        let after = snaps[index].text.clone();
        let before_is_swap = is_strict_swap(&before, committed);
        let after_is_swap = is_strict_swap(&after, committed);
        if before_is_swap {
            swap_target += 1;
        }
        if before == committed {
            before_match += 1;
        }
        if after == committed {
            after_match += 1;
            if before_is_swap {
                swap_after_match += 1;
            }
        }
        if before != committed && after == committed {
            fixed += 1;
            if short {
                short_fixed += 1;
            }
        }
        if before == committed && after != committed {
            broken += 1;
            if short {
                short_broken += 1;
            }
        }
        if before == committed && after_is_swap {
            swap_broken_new += 1;
        }
        if short {
            if before == committed {
                short_before += 1;
            }
            if after == committed {
                short_after += 1;
            }
        }
    }
    println!("history replay under frozen config (counts only):");
    println!("  subset                           : {subset}");
    println!("  targets                          : {target}");
    println!("  skipped                          : {skipped}");
    println!("  top=committed before             : {before_match}");
    println!("  top=committed after              : {after_match}");
    println!("  fixed                            : {fixed}");
    println!("  broken                           : {broken}");
    println!("  strict-swap rows (before!=commit): {swap_target}");
    println!("  of those resolved to committed   : {swap_after_match}");
    println!("  matches turned into a swap       : {swap_broken_new}");
    println!("  readings <=4 chars               : target={short_target} before={short_before} after={short_after} fixed={short_fixed} broken={short_broken}");
}

/// The strict homophone-swap shape: same non-kanji character sequence,
/// kanji differ on both sides.
fn is_strict_swap(first: &str, committed: &str) -> bool {
    fn is_kanji(c: char) -> bool {
        matches!(
            c,
            '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' | '々'
        )
    }
    first != committed
        && first.chars().any(is_kanji)
        && committed.chars().any(is_kanji)
        && first
            .chars()
            .filter(|c| !is_kanji(*c))
            .eq(committed.chars().filter(|c| !is_kanji(*c)))
}

/// Perf mode (#474 condition 8): p50/p99 apply time per conversion on the
/// context corpus, plus process working-set delta for loading the table.
/// Both are outside-engine estimates only.
fn perf(
    service: &ConversionService,
    dictionary: &Dictionary<'_>,
    vocab: &Vocab,
    corpus_dir: &Path,
    table_path: &Path,
) {
    #[cfg(windows)]
    fn working_set() -> u64 {
        use std::mem::size_of;
        #[repr(C)]
        struct ProcessMemoryCounters {
            cb: u32,
            page_fault_count: u32,
            peak_working_set_size: usize,
            working_set_size: usize,
            quota_peak_paged_pool_usage: usize,
            quota_paged_pool_usage: usize,
            quota_peak_nonpaged_pool_usage: usize,
            quota_nonpaged_pool_usage: usize,
            pagefile_usage: usize,
            peak_pagefile_usage: usize,
        }
        extern "system" {
            fn GetCurrentProcess() -> *mut std::ffi::c_void;
            fn GetProcessMemoryInfo(
                process: *mut std::ffi::c_void,
                counters: *mut ProcessMemoryCounters,
                cb: u32,
            ) -> i32;
        }
        let mut counters = ProcessMemoryCounters {
            cb: size_of::<ProcessMemoryCounters>() as u32,
            page_fault_count: 0,
            peak_working_set_size: 0,
            working_set_size: 0,
            quota_peak_paged_pool_usage: 0,
            quota_paged_pool_usage: 0,
            quota_peak_nonpaged_pool_usage: 0,
            quota_nonpaged_pool_usage: 0,
            pagefile_usage: 0,
            peak_pagefile_usage: 0,
        };
        // SAFETY: `counters` is a valid, properly-sized out buffer for the
        // current-process handle; the struct matches the Windows API shape.
        unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut counters,
                size_of::<ProcessMemoryCounters>() as u32,
            );
        }
        counters.working_set_size as u64
    }
    #[cfg(not(windows))]
    fn working_set() -> u64 {
        0
    }

    let before_mem = working_set();
    let table = load_table(vocab, table_path).expect("table");
    let after_mem = working_set();
    println!(
        "table load: {} pairs, {} bytes on disk",
        table.pairs, table.bytes
    );
    println!(
        "working-set delta on load: {} bytes (outside-engine estimate)",
        after_mem.saturating_sub(before_mem)
    );

    let (_, context_rows) = load_corpora(corpus_dir);
    let config = Config {
        window: 0,
        beta: 100,
        lambda_milli: 1000,
        cap: 3000,
        margin: 0,
        top_n: 8,
        intra_candidate: true,
    };
    let stage = Stage {
        service,
        dictionary,
        vocab,
    };
    let mut samples = Vec::with_capacity(context_rows.len() * 4);
    for row in &context_rows {
        let snaps = context_candidates(service, row).expect("context candidates");
        let clues = clues_of(vocab, &row.clue_surface);
        let start = Instant::now();
        let index = apply_association(&stage, &row.reading, &table, &clues, &snaps, &config);
        samples.push(start.elapsed());
        let _ = index;
    }
    samples.sort();
    let p50 = samples[samples.len() / 2];
    let p99 = samples[(samples.len() * 99) / 100];
    println!(
        "apply per conversion: p50 {:?} p99 {:?} (outside-engine estimate)",
        p50, p99
    );
}
