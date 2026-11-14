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
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;

use sakura_core::dictionary::Dictionary;

#[path = "../assoc_vocab.rs"]
mod vocab;

use vocab::{count_text, Counts, Vocab, DISTANCE_CAP};

fn usage() -> ! {
    eprintln!(
        "usage:\n  \
         assoc-table-build count --dictionary <system.dic> --counts <counts.tsv>\n  \
         assoc-table-build emit  --dictionary <system.dic> --counts <counts.tsv> \\\n             \
             --table <table.tsv> --window <chars> --min-pair <n> [--examples]\n  \
         assoc-table-build stats --counts <counts.tsv>"
    );
    std::process::exit(2);
}

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
        let distance: u8 = fields[2].parse().map_err(|_| "distance")?;
        let count: u64 = fields[3].parse().map_err(|_| "count")?;
        *counts.pairs.entry((target, clue, distance)).or_insert(0) += count;
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
    for (&(target, clue, distance), &count) in &counts.pairs {
        out.push_str(&format!("{target}\t{clue}\t{distance}\t{count}\n"));
    }
    fs::write(path, out).map_err(|e| format!("counts write: {e}"))
}

/// Counts restricted to a window: collapses distance buckets below `window`
/// (a gap of `d` chars counts for every W > d).
fn windowed(counts: &Counts, window: usize) -> BTreeMap<(u32, u32), u64> {
    let mut pairs: BTreeMap<(u32, u32), u64> = BTreeMap::new();
    for (&(target, clue, distance), &count) in &counts.pairs {
        if usize::from(distance) < window {
            *pairs.entry((target, clue)).or_insert(0) += count;
        }
    }
    pairs
}

/// Fully expanded statistics for `pairs`: each pair's near count, its
/// target's total clue count (P(c|t) denominator), the same clue's near
/// count summed across every target sharing the reading (P(c|G) numerator),
/// and the group's total clue count (P(c|G) denominator).
struct Stats {
    /// (target, clue) -> near.
    near: BTreeMap<(u32, u32), u64>,
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

fn compute_stats(vocab: &Vocab, pairs: &BTreeMap<(u32, u32), u64>) -> Stats {
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
        near: pairs.clone(),
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
fn weight_of(stats: &Stats, target: u32, clue: u32) -> Option<i64> {
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
            let counts_path =
                PathBuf::from(arg_value(&args, "--counts").unwrap_or_else(|| usage()));
            let bytes = fs::read(&dictionary_path).expect("dictionary read");
            let dictionary = Dictionary::parse(&bytes).expect("dictionary parse");
            let vocab = Vocab::load(&dictionary).expect("vocab");
            eprintln!(
                "vocab: {} entries ({} targets, {} clues)",
                vocab.entries.len(),
                vocab.entries.iter().filter(|e| e.is_target).count(),
                vocab.entries.iter().filter(|e| e.is_clue).count()
            );
            let mut counts = load_counts(&counts_path).expect("counts load");
            let mut input = String::new();
            std::io::stdin().read_to_string(&mut input).expect("stdin");
            count_text(&vocab, &mut counts, &input);
            write_counts(&counts, &counts_path).expect("counts write");
            eprintln!(
                "counts: {} articles, {} paragraphs, {} pairs",
                counts.articles,
                counts.paragraphs,
                counts.pairs.len()
            );
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
                window <= usize::from(DISTANCE_CAP) + 1,
                "window {window} exceeds the counted distance cap {}",
                usize::from(DISTANCE_CAP) + 1
            );
            let bytes = fs::read(&dictionary_path).expect("dictionary read");
            let dictionary = Dictionary::parse(&bytes).expect("dictionary parse");
            let vocab = Vocab::load(&dictionary).expect("vocab");
            let counts = load_counts(&counts_path).expect("counts load");
            let pairs = windowed(&counts, window);
            let stats = compute_stats(&vocab, &pairs);
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
            fs::write(&table_path, &out).expect("table write");
            let digest = <sha2::Sha256 as sha2::Digest>::digest(out.as_bytes());
            eprintln!("table: {} pairs kept of {}", rows.len(), pairs.len());
            eprintln!("table bytes: {}", out.len());
            eprintln!("table sha256: {digest:x}");
            if examples {
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
        for &target in &targets {
            let entry = &vocab.entries[target as usize];
            let mut scored: Vec<(u32, i64)> = pairs
                .keys()
                .filter(|&&(t, _)| t == target)
                .filter_map(|&(t, clue)| weight_of(&stats, t, clue).map(|w| (clue, w)))
                .collect();
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
