//! End-to-end Phase 4 prediction-worker latency gate.

#![cfg(windows)]

use std::ffi::OsString;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use sakura_engine::dictionary;
use sakura_engine::prediction::{PredictionResult, PredictionRuntime};

const DEFAULT_WARMUP: usize = 250;
const DEFAULT_SAMPLES: usize = 2_000;
const LATENCY_BUDGET: Duration = Duration::from_millis(10);
const REQUEST_TIMEOUT: Duration = Duration::from_millis(100);
const PREFIXES: [&str; 8] = [
    "か",
    "かん",
    "かんすう",
    "で",
    "でぷ",
    "でぷろい",
    "おぶざ",
    "おぶざーばびりてぃ",
];

#[derive(Debug)]
struct Options {
    dictionary: PathBuf,
    report: PathBuf,
    warmup: usize,
    samples: usize,
}

#[derive(Debug)]
struct Measurement {
    samples: Vec<Duration>,
    timeouts: usize,
    invalid_results: usize,
}

fn main() {
    if let Err(error) = run(std::env::args_os().skip(1)) {
        eprintln!("prediction-eval: {error}");
        std::process::exit(2);
    }
}

fn run(args: impl Iterator<Item = OsString>) -> Result<(), String> {
    let options = parse_options(args)?;
    let conversion = dictionary::open(&options.dictionary).map_err(|error| error.to_string())?;
    let runtime = PredictionRuntime::start(Arc::clone(&conversion))
        .map_err(|error| format!("start prediction runtime: {error}"))?;
    let service = runtime.service();

    let measured = measure(&service, options.warmup, options.samples);
    let coalesced = service.coalesced_requests();
    let stop_result = runtime.stop();
    if stop_result.is_err() {
        return Err("prediction worker panicked while joining".to_owned());
    }
    let mut measured = measured?;
    measured.samples.sort_unstable();
    let p99 = measured.samples[(measured.samples.len() - 1) * 99 / 100];
    let maximum = measured.samples[measured.samples.len() - 1];
    let passed = measured.timeouts == 0
        && measured.invalid_results == 0
        && coalesced == 0
        && p99 <= LATENCY_BUDGET;
    write_report(
        &options.report,
        &options,
        &measured,
        p99,
        maximum,
        coalesced,
        passed,
    )?;

    println!(
        "prediction p99 {:.3} ms; max {:.3} ms; {} samples; {} timeouts; report: {}",
        p99.as_secs_f64() * 1_000.0,
        maximum.as_secs_f64() * 1_000.0,
        measured.samples.len(),
        measured.timeouts,
        options.report.display()
    );
    if passed {
        Ok(())
    } else {
        Err("one or more Phase 4 prediction latency gates failed".to_owned())
    }
}

fn measure(
    service: &sakura_engine::prediction::PredictionService,
    warmup: usize,
    samples: usize,
) -> Result<Measurement, String> {
    let mut result = PredictionResult::default();
    for sequence in 0..warmup {
        let generation = u64::try_from(sequence).map_err(|_| "warm-up sequence overflow")?;
        let prefix = PREFIXES[sequence % PREFIXES.len()];
        if !service.request_into(1, generation, prefix, 1_000, REQUEST_TIMEOUT, &mut result) {
            return Err(format!(
                "warm-up prediction timed out for prefix '{prefix}'"
            ));
        }
    }

    let mut durations = Vec::with_capacity(samples);
    let mut timeouts = 0usize;
    let mut invalid_results = 0usize;
    for sequence in 0..samples {
        let generation =
            u64::try_from(warmup + sequence).map_err(|_| "sample sequence overflow")?;
        let prefix = PREFIXES[sequence % PREFIXES.len()];
        let started = Instant::now();
        let completed =
            service.request_into(1, generation, prefix, 1_000, REQUEST_TIMEOUT, &mut result);
        durations.push(started.elapsed());
        if !completed {
            timeouts += 1;
        } else if result.session() != 1 || result.generation() != generation {
            invalid_results += 1;
        }
    }
    Ok(Measurement {
        samples: durations,
        timeouts,
        invalid_results,
    })
}

fn parse_options(args: impl Iterator<Item = OsString>) -> Result<Options, String> {
    let mut dictionary = None;
    let mut report = None;
    let mut warmup = DEFAULT_WARMUP;
    let mut samples = DEFAULT_SAMPLES;
    let mut args = args.peekable();
    while let Some(argument) = args.next() {
        let Some(name) = argument.to_str() else {
            return Err("arguments must be valid Unicode".to_owned());
        };
        match name {
            "--dictionary" => set_path(&mut dictionary, next(&mut args, name)?, name)?,
            "--report" => set_path(&mut report, next(&mut args, name)?, name)?,
            "--warmup" => warmup = parse_count(next(&mut args, name)?, name)?,
            "--samples" => samples = parse_count(next(&mut args, name)?, name)?,
            "--help" | "-h" => {
                println!(
                    "Usage: prediction-eval --dictionary FILE --report FILE [--warmup N] [--samples N]"
                );
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument '{name}'")),
        }
    }
    Ok(Options {
        dictionary: dictionary.ok_or("--dictionary is required")?,
        report: report.ok_or("--report is required")?,
        warmup,
        samples,
    })
}

fn next(args: &mut impl Iterator<Item = OsString>, name: &str) -> Result<OsString, String> {
    args.next()
        .ok_or_else(|| format!("{name} requires a value"))
}

fn set_path(slot: &mut Option<PathBuf>, value: OsString, name: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("{name} was specified more than once"));
    }
    *slot = Some(PathBuf::from(value));
    Ok(())
}

fn parse_count(value: OsString, name: &str) -> Result<usize, String> {
    let value = value
        .into_string()
        .map_err(|_| format!("{name} must be valid Unicode"))?;
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("{name} must be an integer"))?;
    if !(1..=1_000_000).contains(&parsed) {
        return Err(format!("{name} must be between 1 and 1000000"));
    }
    Ok(parsed)
}

#[allow(clippy::too_many_arguments)]
fn write_report(
    path: &Path,
    options: &Options,
    measurement: &Measurement,
    p99: Duration,
    maximum: Duration,
    coalesced: u64,
    passed: bool,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let mut json = String::new();
    writeln!(json, "{{").map_err(fmt_error)?;
    writeln!(json, "  \"schema_version\": 1,").map_err(fmt_error)?;
    writeln!(json, "  \"phase\": 4,").map_err(fmt_error)?;
    writeln!(
        json,
        "  \"dictionary\": {},",
        json_string(&options.dictionary.display().to_string())
    )
    .map_err(fmt_error)?;
    writeln!(json, "  \"warmup_samples\": {},", options.warmup).map_err(fmt_error)?;
    writeln!(json, "  \"samples\": {},", measurement.samples.len()).map_err(fmt_error)?;
    writeln!(json, "  \"p99_us\": {},", p99.as_micros()).map_err(fmt_error)?;
    writeln!(json, "  \"max_us\": {},", maximum.as_micros()).map_err(fmt_error)?;
    writeln!(json, "  \"budget_us\": {},", LATENCY_BUDGET.as_micros()).map_err(fmt_error)?;
    writeln!(
        json,
        "  \"request_timeout_us\": {},",
        REQUEST_TIMEOUT.as_micros()
    )
    .map_err(fmt_error)?;
    writeln!(json, "  \"timeouts\": {},", measurement.timeouts).map_err(fmt_error)?;
    writeln!(
        json,
        "  \"invalid_results\": {},",
        measurement.invalid_results
    )
    .map_err(fmt_error)?;
    writeln!(json, "  \"coalesced_requests\": {coalesced},").map_err(fmt_error)?;
    writeln!(json, "  \"prefixes\": [").map_err(fmt_error)?;
    for (index, prefix) in PREFIXES.iter().enumerate() {
        let comma = if index + 1 == PREFIXES.len() { "" } else { "," };
        writeln!(json, "    {}{comma}", json_string(prefix)).map_err(fmt_error)?;
    }
    writeln!(json, "  ],").map_err(fmt_error)?;
    writeln!(json, "  \"passed\": {passed}").map_err(fmt_error)?;
    writeln!(json, "}}").map_err(fmt_error)?;
    std::fs::write(path, json).map_err(|error| format!("write {}: {error}", path.display()))
}

fn json_string(value: &str) -> String {
    let mut output = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character < ' ' => {
                let _ = write!(output, "\\u{:04x}", u32::from(character));
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

fn fmt_error(_: std::fmt::Error) -> String {
    "format prediction report".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_rejects_missing_and_unbounded_counts() {
        assert!(
            parse_options([OsString::from("--samples"), OsString::from("0")].into_iter()).is_err()
        );
        assert!(parse_options(
            [
                OsString::from("--dictionary"),
                OsString::from("system.dic"),
                OsString::from("--report"),
                OsString::from("report.json"),
                OsString::from("--samples"),
                OsString::from("1000001"),
            ]
            .into_iter()
        )
        .is_err());
    }
}
