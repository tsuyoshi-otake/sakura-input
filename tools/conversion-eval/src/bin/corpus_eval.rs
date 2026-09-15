//! Held-out conversion-quality and latency release gate.
//!
//! Mozc is deliberately not executed here. Its top-1 answers are a checked-in
//! input, so CI evaluates the same frozen comparison without installing another
//! IME or allowing an upstream update to move the goalposts.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sakura_core::{ConversionOptions, Converter, Dictionary};

const IMAGE_BUDGET: usize = 128 * 1024 * 1024;
const MATRIX_BUDGET: usize = 4 * 1024 * 1024;
const LATENCY_BUDGET: Duration = Duration::from_millis(20);
const WARMUP_SAMPLES: usize = 250;
const LATENCY_SAMPLES: usize = 2_000;

#[derive(Debug)]
struct Options {
    dictionary: PathBuf,
    corpus: PathBuf,
    baseline: PathBuf,
    report: PathBuf,
    latency_reading: String,
}

#[derive(Debug)]
struct Case {
    id: String,
    slice: Slice,
    reading: String,
    expected: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slice {
    General,
    It,
}

impl Slice {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "general" => Some(Self::General),
            "it" => Some(Self::It),
            _ => None,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::It => "it",
        }
    }
}

#[derive(Debug)]
struct Gap {
    id: String,
    slice: Slice,
    reading: String,
    expected: String,
    sakura: String,
    mozc: String,
}

#[derive(Debug, Default)]
struct Score {
    total: usize,
    sakura_correct: usize,
    mozc_correct: usize,
}

impl Score {
    fn sakura_percent(&self) -> f64 {
        percent(self.sakura_correct, self.total)
    }

    fn mozc_percent(&self) -> f64 {
        percent(self.mozc_correct, self.total)
    }

    fn relative_percent(&self) -> f64 {
        if self.mozc_correct == 0 {
            100.0
        } else {
            percent(self.sakura_correct, self.mozc_correct)
        }
    }
}

#[derive(Debug)]
struct Report {
    image_bytes: usize,
    matrix_bytes: usize,
    overall: Score,
    general: Score,
    it: Score,
    latency_p99: Duration,
    latency_max: Duration,
    gaps: Vec<Gap>,
    passed: bool,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("corpus-eval: {error}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let options = parse_options(std::env::args_os().skip(1))?;
    let dictionary_bytes = std::fs::read(&options.dictionary)
        .map_err(|error| format!("read {}: {error}", options.dictionary.display()))?;
    let dictionary = Dictionary::parse(&dictionary_bytes)
        .map_err(|error| format!("parse {}: {error}", options.dictionary.display()))?;
    let cases = parse_corpus(&options.corpus)?;
    let baseline = parse_baseline(&options.baseline)?;
    validate_baseline(&cases, &baseline)?;

    let mut report = evaluate(
        &dictionary,
        dictionary_bytes.len(),
        &cases,
        &baseline,
        &options.latency_reading,
    )?;
    report.passed = report.image_bytes <= IMAGE_BUDGET
        && report.matrix_bytes <= MATRIX_BUDGET
        && report.overall.sakura_correct * 100 >= report.overall.mozc_correct * 80
        && report.it.sakura_correct * 100 >= report.it.total * 90
        && report.latency_p99 <= LATENCY_BUDGET;
    write_report(&options.report, &report)?;
    println!(
        "held-out: Sakura {}/{} ({:.2}%), Mozc {}/{} ({:.2}%), relative {:.2}%",
        report.overall.sakura_correct,
        report.overall.total,
        report.overall.sakura_percent(),
        report.overall.mozc_correct,
        report.overall.total,
        report.overall.mozc_percent(),
        report.overall.relative_percent(),
    );
    println!(
        "IT: {}/{} ({:.2}%); conversion p99 {:.3} ms; image {} bytes; matrix {} bytes",
        report.it.sakura_correct,
        report.it.total,
        report.it.sakura_percent(),
        report.latency_p99.as_secs_f64() * 1_000.0,
        report.image_bytes,
        report.matrix_bytes,
    );
    println!("report: {}", options.report.display());
    if report.passed {
        Ok(())
    } else {
        Err("one or more Phase 2 release gates failed; inspect the report".to_owned())
    }
}

fn parse_options(args: impl Iterator<Item = std::ffi::OsString>) -> Result<Options, String> {
    let mut dictionary = None;
    let mut corpus = None;
    let mut baseline = None;
    let mut report = None;
    let mut latency_reading = None;
    let mut args = args.peekable();
    while let Some(argument) = args.next() {
        let Some(name) = argument.to_str() else {
            return Err("arguments must be valid Unicode".to_owned());
        };
        let destination = match name {
            "--dictionary" => &mut dictionary,
            "--corpus" => &mut corpus,
            "--baseline" => &mut baseline,
            "--report" => &mut report,
            "--latency-reading" => &mut latency_reading,
            "--help" | "-h" => {
                println!(
                    "Usage: corpus-eval --dictionary FILE --corpus FILE --baseline FILE \\\n+                     --report FILE --latency-reading READING"
                );
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument '{name}'")),
        };
        if destination.is_some() {
            return Err(format!("{name} was specified more than once"));
        }
        *destination = Some(
            args.next()
                .ok_or_else(|| format!("{name} requires a value"))?
                .into_string()
                .map_err(|_| format!("{name} value must be valid Unicode"))?,
        );
    }
    let latency_reading = latency_reading.ok_or("--latency-reading is required")?;
    if latency_reading.chars().count() != 30 {
        return Err("--latency-reading must contain exactly 30 Unicode scalar values".to_owned());
    }
    Ok(Options {
        dictionary: PathBuf::from(dictionary.ok_or("--dictionary is required")?),
        corpus: PathBuf::from(corpus.ok_or("--corpus is required")?),
        baseline: PathBuf::from(baseline.ok_or("--baseline is required")?),
        report: PathBuf::from(report.ok_or("--report is required")?),
        latency_reading,
    })
}

fn parse_corpus(path: &Path) -> Result<Vec<Case>, String> {
    let text = read_utf8(path)?;
    let mut cases = Vec::new();
    let mut ids = BTreeMap::new();
    for (line_index, raw) in text.lines().enumerate() {
        let line_number = line_index + 1;
        let line = raw.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "id\tslice\treading\texpected" {
            continue;
        }
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 4 || fields.iter().any(|field| field.is_empty()) {
            return Err(format!(
                "{}:{line_number}: expected four non-empty TSV fields",
                path.display()
            ));
        }
        let slice = Slice::parse(fields[1]).ok_or_else(|| {
            format!(
                "{}:{line_number}: slice must be 'general' or 'it'",
                path.display()
            )
        })?;
        if ids.insert(fields[0].to_owned(), line_number).is_some() {
            return Err(format!(
                "{}:{line_number}: duplicate id '{}'",
                path.display(),
                fields[0]
            ));
        }
        cases.push(Case {
            id: fields[0].to_owned(),
            slice,
            reading: fields[2].to_owned(),
            expected: fields[3].to_owned(),
        });
    }
    if cases.is_empty()
        || !cases.iter().any(|case| case.slice == Slice::General)
        || !cases.iter().any(|case| case.slice == Slice::It)
    {
        return Err(format!(
            "{} must contain non-empty general and IT slices",
            path.display()
        ));
    }
    Ok(cases)
}

fn parse_baseline(path: &Path) -> Result<BTreeMap<String, String>, String> {
    let text = read_utf8(path)?;
    let mut baseline = BTreeMap::new();
    for (line_index, raw) in text.lines().enumerate() {
        let line_number = line_index + 1;
        let line = raw.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') || line == "id\tmozc_top1" {
            continue;
        }
        let Some((id, top1)) = line.split_once('\t') else {
            return Err(format!(
                "{}:{line_number}: expected id and Mozc top-1",
                path.display()
            ));
        };
        if id.is_empty() || top1.is_empty() || top1.contains('\t') {
            return Err(format!(
                "{}:{line_number}: expected two non-empty TSV fields",
                path.display()
            ));
        }
        if baseline.insert(id.to_owned(), top1.to_owned()).is_some() {
            return Err(format!(
                "{}:{line_number}: duplicate id '{id}'",
                path.display()
            ));
        }
    }
    Ok(baseline)
}

fn validate_baseline(cases: &[Case], baseline: &BTreeMap<String, String>) -> Result<(), String> {
    if cases.len() != baseline.len() {
        return Err(format!(
            "baseline has {} rows but corpus has {}",
            baseline.len(),
            cases.len()
        ));
    }
    for case in cases {
        if !baseline.contains_key(&case.id) {
            return Err(format!("baseline is missing id '{}'", case.id));
        }
    }
    Ok(())
}

fn evaluate(
    dictionary: &Dictionary<'_>,
    image_bytes: usize,
    cases: &[Case],
    baseline: &BTreeMap<String, String>,
    latency_reading: &str,
) -> Result<Report, String> {
    let mut converter = Converter::new();
    let mut overall = Score::default();
    let mut general = Score::default();
    let mut it = Score::default();
    let mut gaps = Vec::new();
    for case in cases {
        let actual = converter
            .convert(dictionary, &case.reading, ConversionOptions::default())
            .map_err(|error| format!("{}: conversion failed: {error}", case.id))?
            .first()
            .ok_or_else(|| format!("{}: conversion returned no candidates", case.id))?
            .text()
            .to_owned();
        let mozc = baseline
            .get(&case.id)
            .ok_or_else(|| format!("baseline is missing id '{}'", case.id))?;
        add_score(&mut overall, &actual, mozc, &case.expected);
        let slice_score = match case.slice {
            Slice::General => &mut general,
            Slice::It => &mut it,
        };
        add_score(slice_score, &actual, mozc, &case.expected);
        if actual != case.expected {
            gaps.push(Gap {
                id: case.id.clone(),
                slice: case.slice,
                reading: case.reading.clone(),
                expected: case.expected.clone(),
                sakura: actual,
                mozc: mozc.clone(),
            });
        }
    }

    for _ in 0..WARMUP_SAMPLES {
        converter
            .convert(dictionary, latency_reading, ConversionOptions::default())
            .map_err(|error| format!("latency warm-up conversion failed: {error}"))?;
    }
    let mut samples = Vec::with_capacity(LATENCY_SAMPLES);
    for _ in 0..LATENCY_SAMPLES {
        let started = Instant::now();
        converter
            .convert(dictionary, latency_reading, ConversionOptions::default())
            .map_err(|error| format!("latency conversion failed: {error}"))?;
        samples.push(started.elapsed());
    }
    samples.sort_unstable();
    let latency_p99 = samples[(samples.len() - 1) * 99 / 100];
    let latency_max = samples[samples.len() - 1];
    Ok(Report {
        image_bytes,
        matrix_bytes: dictionary.matrix_bytes_len(),
        overall,
        general,
        it,
        latency_p99,
        latency_max,
        gaps,
        passed: false,
    })
}

fn add_score(score: &mut Score, sakura: &str, mozc: &str, expected: &str) {
    score.total += 1;
    score.sakura_correct += usize::from(sakura == expected);
    score.mozc_correct += usize::from(mozc == expected);
}

fn percent(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 * 100.0 / denominator as f64
    }
}

fn read_utf8(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn write_report(path: &Path, report: &Report) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    let mut json = String::new();
    writeln!(&mut json, "{{").map_err(fmt_error)?;
    writeln!(&mut json, "  \"schema_version\": 1,").map_err(fmt_error)?;
    writeln!(&mut json, "  \"split\": \"held-out\",").map_err(fmt_error)?;
    writeln!(&mut json, "  \"image_bytes\": {},", report.image_bytes).map_err(fmt_error)?;
    writeln!(&mut json, "  \"image_budget_bytes\": {IMAGE_BUDGET},").map_err(fmt_error)?;
    writeln!(&mut json, "  \"matrix_bytes\": {},", report.matrix_bytes).map_err(fmt_error)?;
    writeln!(&mut json, "  \"matrix_budget_bytes\": {MATRIX_BUDGET},").map_err(fmt_error)?;
    write_score(&mut json, "overall", &report.overall, true)?;
    write_score(&mut json, "general", &report.general, true)?;
    write_score(&mut json, "it", &report.it, true)?;
    writeln!(&mut json, "  \"latency\": {{").map_err(fmt_error)?;
    writeln!(&mut json, "    \"samples\": {LATENCY_SAMPLES},").map_err(fmt_error)?;
    writeln!(
        &mut json,
        "    \"p99_us\": {},",
        report.latency_p99.as_micros()
    )
    .map_err(fmt_error)?;
    writeln!(
        &mut json,
        "    \"max_us\": {},",
        report.latency_max.as_micros()
    )
    .map_err(fmt_error)?;
    writeln!(
        &mut json,
        "    \"budget_us\": {}",
        LATENCY_BUDGET.as_micros()
    )
    .map_err(fmt_error)?;
    writeln!(&mut json, "  }},").map_err(fmt_error)?;
    writeln!(&mut json, "  \"gaps\": [").map_err(fmt_error)?;
    for (index, gap) in report.gaps.iter().enumerate() {
        let comma = if index + 1 == report.gaps.len() {
            ""
        } else {
            ","
        };
        writeln!(
            &mut json,
            "    {{\"id\":{},\"slice\":{},\"reading\":{},\"expected\":{},\"sakura\":{},\"mozc\":{}}}{comma}",
            json_string(&gap.id),
            json_string(gap.slice.name()),
            json_string(&gap.reading),
            json_string(&gap.expected),
            json_string(&gap.sakura),
            json_string(&gap.mozc),
        )
        .map_err(fmt_error)?;
    }
    writeln!(&mut json, "  ],").map_err(fmt_error)?;
    writeln!(&mut json, "  \"passed\": {}", report.passed).map_err(fmt_error)?;
    writeln!(&mut json, "}}").map_err(fmt_error)?;
    std::fs::write(path, json).map_err(|error| format!("write {}: {error}", path.display()))
}

fn write_score(json: &mut String, name: &str, score: &Score, comma: bool) -> Result<(), String> {
    writeln!(json, "  \"{name}\": {{").map_err(fmt_error)?;
    writeln!(json, "    \"total\": {},", score.total).map_err(fmt_error)?;
    writeln!(json, "    \"sakura_correct\": {},", score.sakura_correct).map_err(fmt_error)?;
    writeln!(json, "    \"mozc_correct\": {},", score.mozc_correct).map_err(fmt_error)?;
    writeln!(
        json,
        "    \"sakura_percent\": {:.2},",
        score.sakura_percent()
    )
    .map_err(fmt_error)?;
    writeln!(json, "    \"mozc_percent\": {:.2},", score.mozc_percent()).map_err(fmt_error)?;
    writeln!(
        json,
        "    \"relative_to_mozc_percent\": {:.2}",
        score.relative_percent()
    )
    .map_err(fmt_error)?;
    writeln!(json, "  }}{}", if comma { "," } else { "" }).map_err(fmt_error)
}

fn json_string(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len() + 2);
    encoded.push('"');
    for character in value.chars() {
        match character {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            character if character.is_control() => {
                let _ = write!(&mut encoded, "\\u{:04x}", character as u32);
            }
            character => encoded.push(character),
        }
    }
    encoded.push('"');
    encoded
}

fn fmt_error(_: std::fmt::Error) -> String {
    "formatting an in-memory report failed".to_owned()
}
