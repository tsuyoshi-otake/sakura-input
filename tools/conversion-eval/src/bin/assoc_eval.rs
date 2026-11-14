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
//! assoc-eval grid --dictionary <dic> --table-dir <dir> [--corpus-dir <dir>]
//!     Runs every grid point: W picks table-W{W}.tsv; each point prints
//!     context strict/lenient, one-shot strict/lenient, pair buckets and
//!     conversion-row fixed/broken.
//! assoc-eval run --dictionary <dic> --table <file> --corpus <file>
//!         [--config W,lambda_milli,cap,margin,N,intra]
//!     Applies one frozen configuration to a separate context-format file —
//!     the verifier's held-out pairs (#474 condition 7).
//! assoc-eval replay --dictionary <dic> --table <file> --history <export>
//!         [--config ...]
//!     Replays a developer-history export once under the frozen config,
//!     counts only (target, matches before/after, fixed, broken; strict
//!     homophone-swap and <=4-char-reading breakdowns).
//! assoc-eval perf --dictionary <dic> --table <file>
//!     p50/p99 apply time per conversion and table-load memory — explicit
//!     outside-engine estimates.
//! ```
//!
//! Grid parameter strings are `W,lambda_milli,cap,margin,top_n,intra` where
//! `lambda_milli` is λ × 1000 and `intra` is 0/1 for clue words inside the
//! candidate itself.

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

/// One frozen application configuration (#474 condition 5).
#[derive(Debug, Clone, Copy)]
struct Config {
    /// Clue window in chars — selects the W-suffixed table file.
    #[allow(dead_code)] // read only through file naming, kept for provenance
    window: usize,
    /// λ × 1000 applied to the summed clue weight.
    lambda_milli: i64,
    /// Cap on the summed clue weight per candidate.
    cap: i64,
    /// Adjusted price must beat the original top's by at least this.
    margin: i64,
    /// Swappable candidates sit within the top N.
    top_n: usize,
    /// Also use the other words inside the candidate as clues.
    intra_candidate: bool,
}

impl Config {
    fn parse(text: &str) -> Result<Self, String> {
        let fields: Vec<&str> = text.split(',').collect();
        if fields.len() != 6 {
            return Err(format!(
                "config '{text}' needs 6 fields: W,lambda_milli,cap,margin,N,intra"
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
            lambda_milli: parse_i64(1)?,
            cap: parse_i64(2)?,
            margin: parse_i64(3)?,
            top_n: parse_i64(4)? as usize,
            intra_candidate: parse_i64(5)? != 0,
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

/// The association table for one window: (target entry, clue entry) ->
/// weight, plus for application the surface->entry lookup comes from Vocab.
struct Table {
    weights: HashMap<(u32, u32), i64>,
    pairs: usize,
    bytes: usize,
}

fn load_table(vocab: &Vocab, path: &Path) -> Result<Table, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("table read: {e}"))?;
    let mut weights = HashMap::new();
    let mut pairs = 0usize;
    for (number, line) in text.lines().enumerate() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 4 {
            return Err(format!("table:{}: expected 4 columns", number + 1));
        }
        let (reading, target, clue, weight) = (fields[0], fields[1], fields[2], fields[3]);
        let weight: i64 = weight
            .parse()
            .map_err(|_| format!("table:{}: weight not an integer", number + 1))?;
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
        weights.insert((target_entry, clue_entry), weight);
        pairs += 1;
    }
    Ok(Table {
        weights,
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
            if let Some(&weight) = table.weights.get(&(target, clue)) {
                total += weight;
            }
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
    let score_top = score_candidate(stage.vocab, table, reading, top, clues);
    let score_alt = score_candidate(stage.vocab, table, reading, candidate, clues);
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
            let score = score_candidate(stage.vocab, table, reading, candidate, clues);
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

fn usage() -> ! {
    eprintln!(
        "usage: assoc-eval <baseline|grid|run|replay|perf> --dictionary <system.dic> [options]\n  \
         baseline: [--corpus-dir corpus/accuracy]\n  \
         grid:     --table-dir <dir> [--corpus-dir corpus/accuracy]\n  \
         dump:     --corpus <context.tsv> --out <candidates.jsonl>\n  \
         run:      --table <file> --corpus <file> [--config W,lam,cap,margin,N,intra]\n  \
         replay:   --table <file> --history <export.tsv> [--config ...]\n  \
         perf:     --table <file>"
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
            grid(&service, &dictionary, &vocab, &corpus_dir, &table_dir);
        }
        "run" => {
            let table = PathBuf::from(arg_value(&args, "--table").unwrap_or_else(|| usage()));
            let corpus_path =
                PathBuf::from(arg_value(&args, "--corpus").unwrap_or_else(|| usage()));
            let config = arg_value(&args, "--config")
                .map(|s| Config::parse(&s).expect("config"))
                .unwrap_or(Config {
                    window: 0,
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
                    lambda_milli: 1000,
                    cap: i64::MAX,
                    margin: 0,
                    top_n: 5,
                    intra_candidate: false,
                });
            replay(&service, &dictionary, &vocab, &table, &history, &config);
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

fn grid(
    service: &ConversionService,
    dictionary: &Dictionary<'_>,
    vocab: &Vocab,
    corpus_dir: &Path,
    table_dir: &Path,
) {
    let (conversion_rows, context_rows) = load_corpora(corpus_dir);
    let stage = Stage {
        service,
        dictionary,
        vocab,
    };
    // The grid the Issue fixed in advance: W x lambda x cap x margin x N x intra.
    let windows = [30usize, 60, 120];
    let lambdas = [500i64, 1000, 2000];
    let caps = [1500i64, 3000];
    let margins = [0i64, 500];
    let top_ns = [3usize, 8];
    let intras = [false, true];
    println!(
        "W\tlambda\tcap\tmargin\tN\tintra\tctx_lenient\tctx_strict\toneshot_lenient\toneshot_strict\tpairs_both\tpairs_one\tpairs_neither\tconv_fixed\tconv_broken"
    );
    for &window in &windows {
        let table_path = table_dir.join(format!("table-W{window}.tsv"));
        let table = match load_table(vocab, &table_path) {
            Ok(table) => table,
            Err(error) => {
                eprintln!("W={window}: {error} (skipped)");
                continue;
            }
        };
        for &lambda in &lambdas {
            for &cap in &caps {
                for &margin in &margins {
                    for &top_n in &top_ns {
                        for &intra in &intras {
                            let config = Config {
                                window,
                                lambda_milli: lambda,
                                cap,
                                margin,
                                top_n,
                                intra_candidate: intra,
                            };
                            let (
                                ctx_lenient,
                                ctx_strict,
                                oneshot_lenient,
                                oneshot_strict,
                                pairs_both,
                                pairs_one,
                                pairs_neither,
                                conv_fixed,
                                conv_broken,
                            ) = evaluate(
                                &stage,
                                &conversion_rows,
                                &context_rows,
                                &table,
                                &config,
                                &HashMap::new(),
                            );
                            println!(
                                "{window}\t{lambda}\t{cap}\t{margin}\t{top_n}\t{}\t{ctx_lenient}\t{ctx_strict}\t{oneshot_lenient}\t{oneshot_strict}\t{pairs_both}\t{pairs_one}\t{pairs_neither}\t{conv_fixed}\t{conv_broken}",
                                usize::from(intra),
                            );
                        }
                    }
                }
            }
        }
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
) {
    let table = load_table(vocab, table_path).expect("table");
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
    let stage = Stage {
        service,
        dictionary,
        vocab,
    };
    let mut target = 0usize;
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
    let mut short_target = 0usize;
    let mut short_before = 0usize;
    let mut short_after = 0usize;
    let mut short_fixed = 0usize;
    let mut short_broken = 0usize;
    let mut skipped = 0usize;
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
        // Same rule as the accuracy gate's replay: an unrecoverable previous
        // commit carries nothing, so the row is measured as a no-bridge
        // conversion rather than dropped.
        let (initial_right_id, bridge) =
            commit_carry(service, &clue_reading, &clue_surface).unwrap_or_default();
        let snaps = with_service(|| {
            service.with_conversion_input_bridge_hints(
                ConversionInput::ordinary(fields[indexes.4]),
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
        let committed = fields[indexes.5];
        let before = snaps[0].text.clone();
        let mut clues = clues_of(vocab, &clue_text);
        if config.intra_candidate {
            for skip in 0..snaps[0].segments.len() {
                clues.extend(intra_clues(vocab, fields[indexes.4], &snaps[0], skip));
            }
        }
        let index = apply_association(&stage, fields[indexes.4], &table, &clues, &snaps, config);
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
