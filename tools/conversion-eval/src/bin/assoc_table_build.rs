//! Builds the #474 association table from public Wikipedia text.
//!
//! One `jawiki-*-pages-articles-multistream*.bz2` part at a time goes through
//! the Python extractor into `count`, which folds the part's paragraphs into
//! the checkpoint file; the caller deletes each part after it drains:
//!
//! ```text
//! python -I scripts/extract-wiki-text.py <part.bz2> \
//!     | assoc-table-build count --dictionary <system.dic> --counts <counts.tsv>
//! ```
//!
//! v2 (#474 verification fix): one (target, clue) pair is counted at most
//! once per article (a repeated proper noun can no longer farm weight), and
//! the emitted per-window files carry COUNTS — `near`, `group_near`,
//! `target_total`, `group_total` — instead of baked weights. The beta
//! smoothing
//!   `share(t)   = target_total(t) / group_total(G)`
//!   `P~(t|c)    = (near(c,t) + beta*share(t)) / (group_near(c,G) + beta)`
//!   `w          = 500 * ln(P~/share)`
//! is applied when assoc-eval loads a counts file, so beta changes need no
//! recount. `--examples` prints the ten heaviest clues per target under the
//! given `--beta` for the five public example readings. Counts, dumps and
//! tables live outside the repository (under ~/tmp); nothing here is
//! committed but this tool.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use sakura_core::dictionary::Dictionary;

#[path = "../assoc_vocab.rs"]
mod vocab;

use vocab::{Counts, Vocab};

fn usage() -> ! {
    eprintln!(
        "usage:\n  \
         assoc-table-build count --dictionary <system.dic> [--counts <counts.tsv>]\n             \
             [--emit-prefix <table>] [--min-pair <n>] [--examples] < extracted.txt\n  \
         assoc-table-build emit  --dictionary <system.dic> --counts <counts.tsv> \\\n             \
             --table <table.tsv> --window <chars> --min-pair <n> [--examples]\n  \
         assoc-table-build stats --counts <counts.tsv>"
    );
    std::process::exit(2);
}

/// Memory guard for the counting phase: beyond this many distinct
/// (target, clue, bucket) pairs, singleton pairs are pruned. 230M packed
/// entries in a HashMap stay under the 12 GB tool budget (#474 directive).
const PAIR_HARD_CAP: usize = 230_000_000;
/// The guard runs at most once per this many new paragraphs, so the O(len)
/// scan stays amortized instead of firing on every input line. One scan can
/// drop at most 1 from a pair's recorded count, so any pair's undercount is
/// at most the total number of scans R.
const PRUNE_INTERVAL: u64 = 200_000;
/// Progress report cadence in input lines (about one line per dump part).
const PROGRESS_LINES: u64 = 1_000_000;

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|arg| arg == name)
        .and_then(|at| args.get(at + 1).cloned())
}

fn load_counts(path: &PathBuf) -> Result<Counts, String> {
    let mut counts = Counts::default();
    if !path.exists() {
        return Ok(counts);
    }
    let file = fs::File::open(path).map_err(|e| format!("counts open: {e}"))?;
    for (number, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| format!("counts read: {e}"))?;
        if line.is_empty() {
            continue;
        }
        if line.starts_with('#') {
            for pair in line.split_whitespace().skip(1) {
                let Some((key, value)) = pair.split_once('=') else {
                    continue;
                };
                match key {
                    "articles" => counts.articles = value.parse().unwrap_or(0),
                    "paragraphs" => counts.paragraphs = value.parse().unwrap_or(0),
                    _ => {}
                }
            }
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() == 3 && fields[0] == "T" {
            let target: u32 = fields[1].parse().map_err(|_| "target id")?;
            let count: u64 = fields[2].parse().map_err(|_| "count")?;
            *counts.target_total.entry(target).or_insert(0) += count;
            continue;
        }
        if fields.len() != 4 {
            return Err(format!("counts:{}: expected 4 columns", number + 1));
        }
        let target: u32 = fields[0].parse().map_err(|_| "target id")?;
        let clue: u32 = fields[1].parse().map_err(|_| "clue id")?;
        let bucket: u8 = fields[2].parse().map_err(|_| "bucket")?;
        let count: u64 = fields[3].parse().map_err(|_| "count")?;
        *counts
            .pairs
            .entry(vocab::pack_key(target, clue, bucket))
            .or_insert(0) += count;
    }
    Ok(counts)
}

fn write_counts(counts: &Counts, path: &PathBuf) -> Result<(), String> {
    let mut out = String::with_capacity(1 << 20);
    out.push_str(&format!(
        "# articles={} paragraphs={} pairs={} targets={}\n",
        counts.articles,
        counts.paragraphs,
        counts.pairs.len(),
        counts.target_total.len()
    ));
    let mut totals: Vec<u32> = counts.target_total.keys().copied().collect();
    totals.sort_unstable();
    for target in totals {
        out.push_str(&format!("T\t{target}\t{}\n", counts.target_total[&target]));
    }
    let mut rows: Vec<u64> = counts.pairs.keys().copied().collect();
    rows.sort_unstable();
    for key in rows {
        let (target, clue, bucket) = vocab::unpack_key(key);
        let count = counts.pairs[&key];
        out.push_str(&format!("{target}\t{clue}\t{bucket}\t{count}\n"));
    }
    fs::write(path, out).map_err(|e| format!("counts write: {e}"))
}

/// Counts restricted to a window: keeps every distance bucket fully below
/// `window` (bucket `b` covers gaps in [b*30, b*30+30) for b<2, plus
/// [60,120) for bucket 2), so windows must be grid values 30, 60 or 120.
fn windowed(counts: &Counts, window: usize) -> BTreeMap<(u32, u32), u64> {
    assert!(
        matches!(window, 30 | 60 | 120),
        "window {window} is not a grid value (30/60/120)"
    );
    let mut pairs: BTreeMap<(u32, u32), u64> = BTreeMap::new();
    for (&key, &count) in &counts.pairs {
        let (target, clue, bucket) = vocab::unpack_key(key);
        if usize::from(bucket) < window / 30 {
            *pairs.entry((target, clue)).or_insert(0) += count;
        }
    }
    pairs
}

/// Reading-group key per target: the smallest target entry index under the
/// reading keeps the map cheap while staying deterministic.
fn group_keys(vocab: &Vocab) -> HashMap<u32, u32> {
    let mut reading_key_of: HashMap<&str, u32> = HashMap::new();
    for entry in &vocab.entries {
        if entry.is_target {
            reading_key_of
                .entry(entry.reading.as_str())
                .and_modify(|key| *key = (*key).min(entry.index))
                .or_insert(entry.index);
        }
    }
    let mut key_of_target = HashMap::new();
    for entry in &vocab.entries {
        if entry.is_target {
            key_of_target.insert(entry.index, reading_key_of[entry.reading.as_str()]);
        }
    }
    key_of_target
}

/// Emit one counts file for `window` (#474 v2): rows keep the four numbers
/// the beta-smoothed weight needs — `near` (articles with the pair inside
/// the window), `group_near` (same, summed over the reading's targets),
/// `target_total` (articles containing the target) and `group_total`
/// (summed over the reading's targets). Rows under `min_pair` drop out;
/// output is sorted and its SHA-256 printed to stderr.
fn emit_counts(vocab: &Vocab, counts: &Counts, window: usize, min_pair: u64, path: &Path) {
    let pairs = windowed(counts, window);
    let key_of_target = group_keys(vocab);
    // group_near[(key, clue)] = sum of near over the reading's targets;
    // group_total[key] = sum of target_total over the reading's targets.
    let mut group_near: HashMap<(u32, u32), u64> = HashMap::new();
    let mut group_total: HashMap<u32, u64> = HashMap::new();
    for (&(target, clue), &near) in &pairs {
        *group_near
            .entry((key_of_target[&target], clue))
            .or_insert(0) += near;
    }
    for (&target, &total) in &counts.target_total {
        *group_total.entry(key_of_target[&target]).or_insert(0) += total;
    }
    let mut rows: Vec<(u32, u32, u64, u64, u64, u64)> = Vec::new();
    for (&(target, clue), &near) in &pairs {
        if near < min_pair {
            continue;
        }
        let key = key_of_target[&target];
        rows.push((
            target,
            clue,
            near,
            group_near[&(key, clue)],
            counts.target_total[&target],
            group_total[&key],
        ));
    }
    rows.sort_unstable();
    let mut out = String::with_capacity(rows.len() * 56);
    out.push_str("# reading\ttarget\tclue\tnear\tgroup_near\ttarget_total\tgroup_total\n");
    for (target, clue, near, gn, tt, gt) in &rows {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            vocab.entries[*target as usize].reading,
            vocab.entries[*target as usize].surface,
            vocab.entries[*clue as usize].surface,
            near,
            gn,
            tt,
            gt
        ));
    }
    fs::write(path, &out).expect("counts write");
    let digest = <sha2::Sha256 as sha2::Digest>::digest(out.as_bytes());
    eprintln!(
        "counts {}: {} pairs kept of {}",
        path.display(),
        rows.len(),
        pairs.len()
    );
    eprintln!("counts bytes: {}", out.len());
    eprintln!("counts sha256: {digest:x}");
}

/// The v2 weight at smoothing `beta` (#474): a clue seen only a handful of
/// times lands near the share prior and earns ~0 weight; only frequently
/// repeated co-occurrence earns a large weight.
fn weight_v2(near: u64, group_near: u64, share: f64, beta: f64) -> i64 {
    if share <= 0.0 || group_near == 0 {
        return 0;
    }
    let p_hat = (near as f64 + beta * share) / (group_near as f64 + beta);
    (500.0 * (p_hat / share).ln()).round() as i64
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(mode) = args.get(1).map(String::as_str) else {
        usage();
    };
    match mode {
        "count" => {
            let dictionary_path =
                PathBuf::from(arg_value(&args, "--dictionary").unwrap_or_else(|| usage()));
            // Optional: persist the merged counts as a text checkpoint.
            let counts_path = arg_value(&args, "--counts").map(PathBuf::from);
            // Optional: emit the three grid tables straight after counting, so
            // no large intermediate file is needed under the 1.5 GB budget.
            let emit_prefix = arg_value(&args, "--emit-prefix").map(PathBuf::from);
            let min_pair: u64 = arg_value(&args, "--min-pair")
                .and_then(|v| v.parse().ok())
                .unwrap_or(10);
            let examples = args.iter().any(|arg| arg == "--examples");
            let beta: f64 = arg_value(&args, "--beta")
                .and_then(|v| v.parse().ok())
                .unwrap_or(100.0);
            let bytes = fs::read(&dictionary_path).expect("dictionary read");
            let dictionary = Dictionary::parse(&bytes).expect("dictionary parse");
            let vocab = Vocab::load(&dictionary).expect("vocab");
            eprintln!(
                "vocab: {} entries ({} targets, {} clues)",
                vocab.entries.len(),
                vocab.entries.iter().filter(|e| e.is_target).count(),
                vocab.entries.iter().filter(|e| e.is_clue).count()
            );
            let mut counts = match &counts_path {
                Some(path) => load_counts(path).expect("counts load"),
                None => Counts::default(),
            };
            // Stream lines: an article boundary is a form feed inside a line,
            // each non-empty piece between boundaries is one paragraph.
            // v2: a pair tallies at most once per article, so paragraph
            // counting writes into a per-article accumulator that flushes at
            // each form feed. PAIR_HARD_CAP is the memory guard: past it,
            // pairs seen in a single article so far are dropped (a pair
            // needs near >= min-pair anyway; dropped singles are reported so
            // the table's undercount is on record).
            let stdin = std::io::stdin();
            let reader = std::io::BufReader::with_capacity(1 << 20, stdin.lock());
            let start = std::time::Instant::now();
            let mut acc = vocab::ArticleAcc::default();
            let mut lines_read = 0u64;
            let mut dropped_singletons = 0u64;
            let mut scans = 0u64;
            let mut first_scan_paragraph: Option<u64> = None;
            let mut last_prune_at = 0u64;
            for line in std::io::BufRead::lines(reader) {
                let line = line.expect("stdin line");
                lines_read += 1;
                let mut first = true;
                for piece in line.split('\u{0c}') {
                    if first {
                        first = false;
                    } else {
                        acc.flush(&mut counts);
                        counts.articles += 1;
                    }
                    if !piece.is_empty() {
                        counts.paragraphs += 1;
                        vocab::count_piece(&vocab, &mut acc, piece);
                    }
                }
                // Amortized guard: at most one full scan per PRUNE_INTERVAL
                // paragraphs so the O(len) retain never dominates counting.
                if counts.pairs.len() > PAIR_HARD_CAP
                    && counts.paragraphs - last_prune_at >= PRUNE_INTERVAL
                {
                    last_prune_at = counts.paragraphs;
                    scans += 1;
                    first_scan_paragraph.get_or_insert(counts.paragraphs);
                    counts.pairs.retain(|_, &mut count| {
                        if count == 1 {
                            dropped_singletons += 1;
                            false
                        } else {
                            true
                        }
                    });
                }
                if lines_read.is_multiple_of(PROGRESS_LINES) {
                    eprintln!(
                        "progress: paragraphs={} pairs={} scans={} elapsed={}s",
                        counts.paragraphs,
                        counts.pairs.len(),
                        scans,
                        start.elapsed().as_secs()
                    );
                }
            }
            acc.flush(&mut counts);
            counts.articles += 1;
            eprintln!(
                "counts: {} articles, {} paragraphs, {} pairs in {}s",
                counts.articles,
                counts.paragraphs,
                counts.pairs.len(),
                start.elapsed().as_secs()
            );
            // Error bound for the report: a pair loses at most 1 per scan, so
            // the recorded count undercounts by at most `scans`; with
            // --min-pair 10 every pair whose true count is >= 10 + scans is
            // guaranteed to survive.
            eprintln!(
                "guard: scans={} dropped={} first_scan_paragraph={}",
                scans,
                dropped_singletons,
                first_scan_paragraph.unwrap_or(0)
            );
            if let Some(path) = &counts_path {
                write_counts(&counts, path).expect("counts write");
            }
            if let Some(prefix) = &emit_prefix {
                for window in [30usize, 60, 120] {
                    let path = PathBuf::from(format!("{}-W{window}.tsv", prefix.display()));
                    emit_counts(&vocab, &counts, window, min_pair, &path);
                }
                if examples {
                    let pairs = windowed(&counts, 120);
                    print_examples(&vocab, &counts, &pairs, beta);
                }
            }
        }
        "emit" => {
            let dictionary_path =
                PathBuf::from(arg_value(&args, "--dictionary").unwrap_or_else(|| usage()));
            let counts_path =
                PathBuf::from(arg_value(&args, "--counts").unwrap_or_else(|| usage()));
            let table_path = PathBuf::from(arg_value(&args, "--table").unwrap_or_else(|| usage()));
            let window: usize = arg_value(&args, "--window")
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| usage());
            let min_pair: u64 = arg_value(&args, "--min-pair")
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| usage());
            let examples = args.iter().any(|arg| arg == "--examples");
            let beta: f64 = arg_value(&args, "--beta")
                .and_then(|v| v.parse().ok())
                .unwrap_or(100.0);
            assert!(
                matches!(window, 30 | 60 | 120),
                "window {window} is not a grid value (30/60/120)"
            );
            let bytes = fs::read(&dictionary_path).expect("dictionary read");
            let dictionary = Dictionary::parse(&bytes).expect("dictionary parse");
            let vocab = Vocab::load(&dictionary).expect("vocab");
            let counts = load_counts(&counts_path).expect("counts load");
            emit_counts(&vocab, &counts, window, min_pair, &table_path);
            if examples {
                let pairs = windowed(&counts, window);
                print_examples(&vocab, &counts, &pairs, beta);
            }
        }
        "stats" => {
            let counts_path =
                PathBuf::from(arg_value(&args, "--counts").unwrap_or_else(|| usage()));
            let counts = load_counts(&counts_path).expect("counts load");
            println!("articles={}", counts.articles);
            println!("paragraphs={}", counts.paragraphs);
            println!("pairs={}", counts.pairs.len());
            println!("targets={}", counts.target_total.len());
        }
        _ => usage(),
    }
}

/// The five public example readings (#474 condition 2): ten heaviest clues
/// per target word under the v2 beta-smoothed weight, printed for human
/// review.
fn print_examples(vocab: &Vocab, counts: &Counts, pairs: &BTreeMap<(u32, u32), u64>, beta: f64) {
    const READINGS: [&str; 5] = ["せいど", "しよう", "きのう", "こうせい", "いどう"];
    let key_of_target = group_keys(vocab);
    let mut group_near: HashMap<(u32, u32), u64> = HashMap::new();
    let mut group_total: HashMap<u32, u64> = HashMap::new();
    for (&(target, clue), &near) in pairs {
        *group_near
            .entry((key_of_target[&target], clue))
            .or_insert(0) += near;
    }
    for (&target, &total) in &counts.target_total {
        *group_total.entry(key_of_target[&target]).or_insert(0) += total;
    }
    for reading in READINGS {
        let targets: Vec<u32> = vocab
            .entries
            .iter()
            .filter(|e| e.is_target && e.reading == reading)
            .map(|e| e.index)
            .collect();
        println!("=== {reading} ({} target words) ===", targets.len());
        let target_set: std::collections::HashSet<u32> = targets.iter().copied().collect();
        let mut per_target: HashMap<u32, Vec<(u32, i64)>> = HashMap::new();
        for (&(target, clue), &near) in pairs {
            if !target_set.contains(&target) {
                continue;
            }
            let key = key_of_target[&target];
            let share = counts.target_total[&target] as f64 / group_total[&key] as f64;
            let weight = weight_v2(near, group_near[&(key, clue)], share, beta);
            per_target.entry(target).or_default().push((clue, weight));
        }
        for &target in &targets {
            let entry = &vocab.entries[target as usize];
            let mut scored = per_target.remove(&target).unwrap_or_default();
            scored.sort_by_key(|entry| std::cmp::Reverse(entry.1));
            println!("  {}:", entry.surface);
            if scored.is_empty() {
                println!("    (no clues)");
            }
            for (clue, weight) in scored.iter().take(10) {
                println!("    {weight:>6}  {}", vocab.entries[*clue as usize].surface);
            }
        }
    }
}
