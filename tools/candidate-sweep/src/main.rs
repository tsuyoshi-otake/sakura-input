//! Sweep the conversion candidate limit and report what each limit costs.
//!
//! Issue #95 asks whether Sakura Input can widen its candidate list toward the
//! count a commercial IME shows. `ConversionOptions::max_candidates` is not a
//! display cap: it bounds the n-best search itself, so the answer depends on
//! how search cost grows with the limit and on whether the extra slots reach
//! surfaces the dictionary already holds. This tool measures both against a
//! real dictionary image and prints one TSV row per (reading, limit).
//!
//! It is an offline evaluator. It never links into the shipping engine, adds
//! no registry dependency, and reads only a dictionary and a reading list.

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use sakura_core::{
    ConversionMethod, ConversionOptions, ConversionSearchTerminal, Converter, Dictionary,
    MAX_CONVERSION_CANDIDATES,
};

/// Readings and limits both stay small enough that one run is interactive.
const MAX_READINGS: usize = 4_096;
const MAX_LIMITS: usize = 64;
const DEFAULT_REPEATS: usize = 25;
const DEFAULT_WARMUPS: usize = 5;

struct Cli {
    dictionary: PathBuf,
    readings: PathBuf,
    limits: Vec<usize>,
    repeats: usize,
    warmups: usize,
    it_bias: bool,
    output: Option<PathBuf>,
}

fn usage() -> String {
    format!(
        "usage: candidate-sweep --dictionary <system.dic> --readings <file> \
         --limits 9,18,36 [--repeats {DEFAULT_REPEATS}] [--warmups {DEFAULT_WARMUPS}] \
         --it-bias on|off [--output <file.tsv>]\n\n\
         The reading file holds one reading per line; blank lines and lines \
         starting with a hash are ignored. This build accepts limits up to \
         {MAX_CONVERSION_CANDIDATES}."
    )
}

fn parse_cli() -> Result<Cli, String> {
    let mut dictionary = None;
    let mut readings = None;
    let mut limits = None;
    let mut repeats = DEFAULT_REPEATS;
    let mut warmups = DEFAULT_WARMUPS;
    let mut it_bias = None;
    let mut output = None;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let mut value = || {
            args.next()
                .ok_or_else(|| format!("{flag} requires a value\n\n{}", usage()))
        };
        match flag.as_str() {
            "--dictionary" => dictionary = Some(PathBuf::from(value()?)),
            "--readings" => readings = Some(PathBuf::from(value()?)),
            "--output" => output = Some(PathBuf::from(value()?)),
            "--limits" => {
                let raw = value()?;
                let mut parsed = Vec::new();
                for part in raw.split(',') {
                    let part = part.trim();
                    if part.is_empty() {
                        continue;
                    }
                    let limit: usize = part
                        .parse()
                        .map_err(|_| format!("--limits value is not a number: {part}"))?;
                    if limit == 0 || limit > MAX_CONVERSION_CANDIDATES {
                        return Err(format!(
                            "--limits value {limit} is outside 1..={MAX_CONVERSION_CANDIDATES}; \
                             build with --features wide to sweep past the wire limit"
                        ));
                    }
                    parsed.push(limit);
                }
                if parsed.is_empty() || parsed.len() > MAX_LIMITS {
                    return Err(format!("--limits must name 1..={MAX_LIMITS} limits"));
                }
                parsed.sort_unstable();
                parsed.dedup();
                limits = Some(parsed);
            }
            "--repeats" => {
                repeats = value()?
                    .parse()
                    .map_err(|_| "--repeats is not a number".to_owned())?;
                if repeats == 0 || repeats > 10_000 {
                    return Err("--repeats must be 1..=10000".to_owned());
                }
            }
            "--warmups" => {
                warmups = value()?
                    .parse()
                    .map_err(|_| "--warmups is not a number".to_owned())?;
                if warmups > 10_000 {
                    return Err("--warmups must be 0..=10000".to_owned());
                }
            }
            "--it-bias" => {
                it_bias = Some(match value()?.as_str() {
                    "on" => true,
                    "off" => false,
                    other => return Err(format!("--it-bias must be on or off, got {other}")),
                })
            }
            "--help" | "-h" => return Err(usage()),
            other => return Err(format!("unknown flag {other}\n\n{}", usage())),
        }
    }
    Ok(Cli {
        dictionary: dictionary.ok_or_else(|| format!("--dictionary is required\n\n{}", usage()))?,
        readings: readings.ok_or_else(|| format!("--readings is required\n\n{}", usage()))?,
        limits: limits.ok_or_else(|| format!("--limits is required\n\n{}", usage()))?,
        repeats,
        warmups,
        it_bias: it_bias.ok_or_else(|| format!("--it-bias is required\n\n{}", usage()))?,
        output,
    })
}

fn parse_readings(text: &str) -> Result<Vec<String>, String> {
    let mut readings = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if readings.len() == MAX_READINGS {
            return Err(format!("reading file exceeds {MAX_READINGS} entries"));
        }
        readings.push(line.to_owned());
    }
    if readings.is_empty() {
        return Err("reading file holds no readings".to_owned());
    }
    Ok(readings)
}

fn options_for(limit: usize, it_bias: bool) -> ConversionOptions {
    let mut options = ConversionOptions {
        max_candidates: limit,
        method: ConversionMethod::MultiSegment,
        initial_right_id: 0,
        // The sweep isolates the candidate limit. Repair is a separate bounded
        // pass with its own candidate budget and would blur what is measured.
        skip_input_repair: true,
        ..ConversionOptions::default()
    };
    if !it_bias {
        options.it_bias_per_mille = 0;
        options.max_it_boost = 0;
    }
    options
}

fn terminal_name(terminal: ConversionSearchTerminal) -> &'static str {
    match terminal {
        ConversionSearchTerminal::SearchExhausted => "exhausted",
        ConversionSearchTerminal::CandidateLimitReached => "candidate_limit",
        ConversionSearchTerminal::LatticeBudgetReached => "lattice_budget",
        ConversionSearchTerminal::StateBudgetReached => "state_budget",
    }
}

/// One measured (reading, limit) cell.
struct Row {
    reading: String,
    chars: usize,
    limit: usize,
    candidates: usize,
    single_char_surfaces: usize,
    lattice_nodes: usize,
    states_pushed: usize,
    terminal: &'static str,
    min_us: u128,
    median_us: u128,
    p95_us: u128,
    top1: String,
}

fn percentile(sorted: &[u128], per_mille: u128) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let index = ((sorted.len() as u128 - 1) * per_mille / 1_000) as usize;
    sorted[index]
}

fn run() -> Result<(), String> {
    let cli = parse_cli()?;
    let dictionary_bytes = fs::read(&cli.dictionary)
        .map_err(|error| format!("read {}: {error}", cli.dictionary.display()))?;
    let dictionary = Dictionary::parse(&dictionary_bytes)
        .map_err(|error| format!("parse {}: {error:?}", cli.dictionary.display()))?;
    let readings_text = fs::read_to_string(&cli.readings)
        .map_err(|error| format!("read {}: {error}", cli.readings.display()))?;
    let readings = parse_readings(&readings_text)?;

    let mut converter = Converter::new();
    let mut rows = Vec::with_capacity(readings.len() * cli.limits.len());
    let mut samples = Vec::with_capacity(cli.repeats);

    for &limit in &cli.limits {
        let options = options_for(limit, cli.it_bias);
        for reading in &readings {
            for _ in 0..cli.warmups {
                converter
                    .convert_detailed(&dictionary, reading, options)
                    .map_err(|error| format!("convert {reading} at {limit}: {error:?}"))?;
            }
            samples.clear();
            for _ in 0..cli.repeats {
                let started = Instant::now();
                let result = converter
                    .convert_detailed(&dictionary, reading, options)
                    .map_err(|error| format!("convert {reading} at {limit}: {error:?}"))?;
                let elapsed = started.elapsed().as_micros();
                // Keep the result observed so the timed call cannot be elided.
                std::hint::black_box(result.candidates().len());
                samples.push(elapsed);
            }
            let result = converter
                .convert_detailed(&dictionary, reading, options)
                .map_err(|error| format!("convert {reading} at {limit}: {error:?}"))?;
            let diagnostics = result.diagnostics();
            let candidates = result.candidates();
            let single_char_surfaces = candidates
                .iter()
                .filter(|candidate| candidate.text().chars().count() == 1)
                .count();
            let top1 = candidates
                .first()
                .map(|candidate| candidate.text().to_owned())
                .unwrap_or_default();
            let candidate_count = candidates.len();
            samples.sort_unstable();
            rows.push(Row {
                reading: reading.clone(),
                chars: reading.chars().count(),
                limit,
                candidates: candidate_count,
                single_char_surfaces,
                lattice_nodes: diagnostics.lattice_nodes,
                states_pushed: diagnostics.states_pushed,
                terminal: terminal_name(diagnostics.terminal),
                min_us: samples[0],
                median_us: percentile(&samples, 500),
                p95_us: percentile(&samples, 950),
                top1,
            });
        }
    }

    let mut tsv = String::with_capacity(rows.len() * 96);
    tsv.push_str(
        "reading\tchars\tlimit\tcandidates\tsingle_char\tlattice_nodes\tstates_pushed\t\
         terminal\tmin_us\tmedian_us\tp95_us\ttop1\n",
    );
    for row in &rows {
        tsv.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            row.reading,
            row.chars,
            row.limit,
            row.candidates,
            row.single_char_surfaces,
            row.lattice_nodes,
            row.states_pushed,
            row.terminal,
            row.min_us,
            row.median_us,
            row.p95_us,
            row.top1,
        ));
    }
    match &cli.output {
        Some(path) => {
            fs::write(path, &tsv).map_err(|error| format!("write {}: {error}", path.display()))?
        }
        None => print!("{tsv}"),
    }

    // A per-limit roll-up is what the cap decision actually reads.
    eprintln!("\nlimit\treadings\tmax_cand\tmedian_us_max\tp95_us_max\tstates_max");
    for &limit in &cli.limits {
        let cells: Vec<&Row> = rows.iter().filter(|row| row.limit == limit).collect();
        let max_candidates = cells.iter().map(|row| row.candidates).max().unwrap_or(0);
        let median_max = cells.iter().map(|row| row.median_us).max().unwrap_or(0);
        let p95_max = cells.iter().map(|row| row.p95_us).max().unwrap_or(0);
        let states_max = cells.iter().map(|row| row.states_pushed).max().unwrap_or(0);
        eprintln!(
            "{limit}\t{}\t{max_candidates}\t{median_max}\t{p95_max}\t{states_max}",
            cells.len()
        );
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("candidate-sweep: {error}");
            ExitCode::FAILURE
        }
    }
}
