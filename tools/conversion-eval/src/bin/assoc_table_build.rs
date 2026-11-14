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
//! After the last part, `emit` applies the Issue's weight
//! `w(c,t) = 500 * ln(P(c|t)/P(c|G))` at window W, drops pairs under the
//! pair threshold, sorts the output (deterministic table for a fixed dump),
//! and prints the counts the Issue asks for. `--examples` prints the ten
//! heaviest clues for each of the five public example readings. Counts,
//! dumps and tables live outside the repository (under ~/tmp); nothing here
//! is committed but this tool.

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
        "# articles={} paragraphs={} pairs={}\n",
        counts.articles,
        counts.paragraphs,
        counts.pairs.len()
    ));
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

/// Emit one table for `window`: apply min-pair, keep positive weights only,
/// write sorted rows and print size/SHA-256 to stderr.
fn emit_table(vocab: &Vocab, counts: &Counts, window: usize, min_pair: u64, path: &Path) {
    let pairs = windowed(counts, window);
    let stats = compute_stats(vocab, &pairs);
    let mut rows: Vec<(u32, u32, i64)> = Vec::new();
    for &(target, clue) in pairs.keys() {
        let near = pairs[&(target, clue)];
        if near < min_pair {
            continue;
        }
        if let Some(weight) = weight_of(&stats, target, clue) {
            if weight > 0 {
                rows.push((target, clue, weight));
            }
        }
    }
    rows.sort_unstable();
    let mut out = String::with_capacity(rows.len() * 40);
    out.push_str("# reading\ttarget\tclue\tweight\n");
    for (target, clue, weight) in &rows {
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            vocab.entries[*target as usize].reading,
            vocab.entries[*target as usize].surface,
            vocab.entries[*clue as usize].surface,
            weight
        ));
    }
    fs::write(path, &out).expect("table write");
    let digest = <sha2::Sha256 as sha2::Digest>::digest(out.as_bytes());
    eprintln!(
        "table {}: {} pairs kept of {}",
        path.display(),
        rows.len(),
        pairs.len()
    );
    eprintln!("table bytes: {}", out.len());
    eprintln!("table sha256: {digest:x}");
}

/// Fully expanded statistics for `pairs`: each pair's near count, its
/// target's total clue count (P(c|t) denominator), the same clue's near
/// count summed across every target sharing the reading (P(c|G) numerator),
/// and the group's total clue count (P(c|G) denominator).
struct Stats<'a> {
    /// (target, clue) -> near.
    near: &'a BTreeMap<(u32, u32), u64>,
    /// target -> sum of near.
    target_total: HashMap<u32, u64>,
    /// (reading-key, clue) -> near summed across the reading's targets.
    group_near: HashMap<(u32, u32), u64>,
    /// reading-key -> sum of near.
    group_total: HashMap<u32, u64>,
    /// reading key -> reading string (keys are the first target's index).
    reading_key: HashMap<u32, String>,
    /// target entry -> its reading group key.
    key_of_target: HashMap<u32, u32>,
}

fn compute_stats<'a>(vocab: &Vocab, pairs: &'a BTreeMap<(u32, u32), u64>) -> Stats<'a> {
    // Reading group key: the smallest entry index under the reading keeps
    // the map cheap while staying deterministic.
    let mut reading_key_of: HashMap<&str, u32> = HashMap::new();
    for entry in &vocab.entries {
        if entry.is_target {
            reading_key_of
                .entry(entry.reading.as_str())
                .and_modify(|key| *key = (*key).min(entry.index))
                .or_insert(entry.index);
        }
    }
    let mut stats = Stats {
        near: pairs,
        target_total: HashMap::new(),
        group_near: HashMap::new(),
        group_total: HashMap::new(),
        reading_key: HashMap::new(),
        key_of_target: HashMap::new(),
    };
    for (&(target, clue), &count) in pairs {
        *stats.target_total.entry(target).or_insert(0) += count;
        let entry = &vocab.entries[target as usize];
        let key = reading_key_of[entry.reading.as_str()];
        stats.key_of_target.insert(target, key);
        stats
            .reading_key
            .entry(key)
            .or_insert_with(|| entry.reading.clone());
        *stats.group_near.entry((key, clue)).or_insert(0) += count;
        *stats.group_total.entry(key).or_insert(0) += count;
    }
    stats
}

/// w(c,t) in milli-nat-ish dictionary cost units: 500 * ln(P(c|t)/P(c|G)).
fn weight_of(stats: &Stats<'_>, target: u32, clue: u32) -> Option<i64> {
    let near = *stats.near.get(&(target, clue))?;
    let target_total = *stats.target_total.get(&target)?;
    let key = *stats.key_of_target.get(&target)?;
    let group_near = *stats.group_near.get(&(key, clue))?;
    let group_total = *stats.group_total.get(&key)?;
    if target_total == 0 || group_total == 0 || group_near == 0 {
        return None;
    }
    let p_t = near as f64 / target_total as f64;
    let p_g = group_near as f64 / group_total as f64;
    Some((500.0 * (p_t / p_g).ln()).round() as i64)
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
            // PAIR_HARD_CAP is the memory guard: past it, pairs seen once are
            // dropped (a pair needs near >= min-pair anyway; dropped singles
            // are reported so the table's undercount is on record).
            let stdin = std::io::stdin();
            let reader = std::io::BufReader::with_capacity(1 << 20, stdin.lock());
            let start = std::time::Instant::now();
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
                        counts.articles += 1;
                    }
                    if !piece.is_empty() {
                        counts.paragraphs += 1;
                        vocab::count_paragraph(&vocab, &mut counts, &piece);
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
                if lines_read % PROGRESS_LINES == 0 {
                    eprintln!(
                        "progress: paragraphs={} pairs={} scans={} elapsed={}s",
                        counts.paragraphs,
                        counts.pairs.len(),
                        scans,
                        start.elapsed().as_secs()
                    );
                }
            }
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
                    emit_table(&vocab, &counts, window, min_pair, &path);
                }
                if examples {
                    let pairs = windowed(&counts, 120);
                    print_examples(&vocab, &pairs);
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
            assert!(
                matches!(window, 30 | 60 | 120),
                "window {window} is not a grid value (30/60/120)"
            );
            let bytes = fs::read(&dictionary_path).expect("dictionary read");
            let dictionary = Dictionary::parse(&bytes).expect("dictionary parse");
            let vocab = Vocab::load(&dictionary).expect("vocab");
            let counts = load_counts(&counts_path).expect("counts load");
            emit_table(&vocab, &counts, window, min_pair, &table_path);
            if examples {
                let pairs = windowed(&counts, window);
                print_examples(&vocab, &pairs);
            }
        }
        "stats" => {
            let counts_path =
                PathBuf::from(arg_value(&args, "--counts").unwrap_or_else(|| usage()));
            let counts = load_counts(&counts_path).expect("counts load");
            println!("articles={}", counts.articles);
            println!("paragraphs={}", counts.paragraphs);
            println!("pairs={}", counts.pairs.len());
        }
        _ => usage(),
    }
}

/// The five public example readings (#474 condition 2): ten heaviest clues
/// per target word, printed for human review.
fn print_examples(vocab: &Vocab, pairs: &BTreeMap<(u32, u32), u64>) {
    const READINGS: [&str; 5] = ["せいど", "しよう", "きのう", "こうせい", "いどう"];
    let stats = compute_stats(vocab, pairs);
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
        for &(target, clue) in stats.near.keys() {
            if !target_set.contains(&target) {
                continue;
            }
            if let Some(weight) = weight_of(&stats, target, clue) {
                per_target.entry(target).or_default().push((clue, weight));
            }
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
