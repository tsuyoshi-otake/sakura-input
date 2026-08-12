//! Reproducible Top-1 comparison of dictionary conversion and the optional
//! local neural worker.
//!
//! This is an offline evaluation tool.  It only accepts an explicitly supplied
//! static corpus and never connects to an engine, input history, or user
//! dictionary.  The corpus format deliberately reuses the frozen Phase 2
//! `id<TAB>slice<TAB>reading<TAB>expected` contract.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use sakura_core::{ConversionCandidate, ConversionOptions, Converter, Dictionary};
use serde::Serialize;

const MAX_CANDIDATES: usize = 6;
const MAX_FRAME_BYTES: usize = 32 * 1024;
const REQUEST_MAGIC: u32 = 0x524E_4B53; // SKNR
const RESPONSE_MAGIC: u32 = 0x534E_4B53; // SKNS
const PROTOCOL_VERSION: u16 = 1;
const MINIMUM_LONG_READING_CHARS: usize = 10;
const MINIMUM_SEGMENTED_READING_CHARS: usize = 3;
/// A quality-acceptance comparison needs enough independently reviewed cases
/// to make a one-row win or loss meaningful. Smaller corpora are permitted
/// only for evaluator/worker smoke checks.
const MINIMUM_ACCEPTANCE_CASES: usize = 600;
const MINIMUM_CHAT_CASES: usize = 200;
const MINIMUM_EMAIL_CASES: usize = 200;
const MODEL_PENALTY_PER_NAT: f32 = 240.0;
const MAXIMUM_MODEL_PENALTY: i64 = 1_200;

#[derive(Debug)]
struct Options {
    dictionary: PathBuf,
    corpus: PathBuf,
    worker: PathBuf,
    model_directory: PathBuf,
    report: PathBuf,
    mode: Mode,
    timeout: Duration,
    exploratory: bool,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Mode {
    Long,
    AllNormal,
}

impl Mode {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "long" => Some(Self::Long),
            "all-normal" => Some(Self::AllNormal),
            _ => None,
        }
    }
}

#[derive(Debug)]
struct Case {
    id: String,
    slice: String,
    reading: String,
    expected: String,
}

#[derive(Debug, Clone)]
struct SnapshotCandidate {
    fingerprint: u64,
    text: String,
    local_cost: i64,
}

#[derive(Debug, Serialize)]
struct Report {
    schema_version: u32,
    evaluator: &'static str,
    mode: Mode,
    corpus_contract: &'static str,
    cases: usize,
    minimum_acceptance_cases: usize,
    minimum_chat_cases: usize,
    minimum_email_cases: usize,
    acceptance_eligible: bool,
    exploratory: bool,
    slice_counts: BTreeMap<String, usize>,
    baseline: Top1Score,
    neural: NeuralScore,
    comparisons: ComparisonScore,
    /// Candidate generation is shared by baseline and every reranker. Keep
    /// this metric separate from Top-1 so a reranker cannot be credited for a
    /// surface that the dictionary never produced.
    candidate_rank: CandidateRankScore,
    /// Quality slices are emitted in the report rather than reconstructed from
    /// rows, which keeps the 600-case acceptance result easy to audit.
    slice_metrics: BTreeMap<String, SliceScore>,
    rows: Vec<Row>,
}

#[derive(Debug, Default, Serialize)]
struct Top1Score {
    evaluated: usize,
    correct: usize,
}

#[derive(Debug, Default, Serialize)]
struct NeuralScore {
    /// Every parsed row is counted, including rows where the selected mode
    /// deliberately keeps the baseline because the neural gate is ineligible.
    /// This keeps the Top-1 denominator comparable with `baseline`.
    evaluated: usize,
    eligible: usize,
    attempted: usize,
    applied: usize,
    fallback: usize,
    correct: usize,
}

#[derive(Debug, Default, Serialize)]
struct ComparisonScore {
    wins: usize,
    losses: usize,
    ties: usize,
}

#[derive(Debug, Default, Serialize)]
struct CandidateRankScore {
    evaluated: usize,
    recall_at_6: usize,
    /// Sum of reciprocal ranks in thousandths. An absent candidate contributes
    /// zero; divide by `evaluated` to obtain MRR@6 without floating-point
    /// nondeterminism in the JSON artifact.
    mrr_milli_sum: u64,
}

impl CandidateRankScore {
    fn observe(&mut self, candidates: &[SnapshotCandidate], expected: &str) {
        self.evaluated += 1;
        let Some(rank) = candidates
            .iter()
            .position(|candidate| candidate.text == expected)
        else {
            return;
        };
        self.recall_at_6 += 1;
        self.mrr_milli_sum += 1_000 / (rank as u64 + 1);
    }
}

#[derive(Debug, Default, Serialize)]
struct SliceScore {
    cases: usize,
    baseline_correct: usize,
    neural_correct: usize,
    eligible: usize,
    applied: usize,
    fallback: usize,
    candidate_recall_at_6: usize,
    candidate_mrr_milli_sum: u64,
    wins: usize,
    losses: usize,
    ties: usize,
}

#[derive(Debug, Serialize)]
struct Row {
    id: String,
    slice: String,
    reading: String,
    expected: String,
    baseline_top1: String,
    baseline_correct: bool,
    eligible: bool,
    eligibility: &'static str,
    neural_status: &'static str,
    worker_error: Option<String>,
    neural_top1: String,
    neural_correct: bool,
    comparison: &'static str,
}

#[derive(Debug)]
struct WorkerResponse {
    request_id: u64,
    status: u16,
    scores: Vec<(u64, f32)>,
}

#[derive(Debug)]
struct WorkerClient {
    child: Child,
    input: ChildStdin,
    responses: mpsc::Receiver<io::Result<WorkerResponse>>,
    reader: Option<JoinHandle<()>>,
}

impl WorkerClient {
    fn start(worker: &Path, model_directory: &Path) -> Result<Self, String> {
        let mut child = Command::new(worker)
            .arg("--stdio")
            .arg("--model-dir")
            .arg(model_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("start {}: {error}", worker.display()))?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| "worker stdin was not piped".to_owned())?;
        let mut output = child
            .stdout
            .take()
            .ok_or_else(|| "worker stdout was not piped".to_owned())?;
        let (send, responses) = mpsc::channel();
        let reader = thread::Builder::new()
            .name("neural-eval-response".to_owned())
            .spawn(move || loop {
                let response = read_response(&mut output);
                let terminal = response.is_err();
                if send.send(response).is_err() || terminal {
                    break;
                }
            })
            .map_err(|error| format!("start worker response reader: {error}"))?;
        Ok(Self {
            child,
            input,
            responses,
            reader: Some(reader),
        })
    }

    fn score(
        &mut self,
        request_id: u64,
        candidates: &[SnapshotCandidate],
        timeout: Duration,
    ) -> Result<Vec<(u64, f32)>, String> {
        let frame = encode_request(request_id, candidates)?;
        self.input
            .write_all(&frame)
            .map_err(|error| format!("write worker request: {error}"))?;
        self.input
            .flush()
            .map_err(|error| format!("flush worker request: {error}"))?;
        let response = self
            .responses
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mpsc::RecvTimeoutError::Timeout => "worker response timed out".to_owned(),
                mpsc::RecvTimeoutError::Disconnected => {
                    "worker response stream disconnected".to_owned()
                }
            })?;
        let response = response.map_err(|error| format!("read worker response: {error}"))?;
        if response.request_id != request_id {
            return Err("worker response request id does not match".to_owned());
        }
        if response.status != 0 {
            return Err(format!(
                "worker reported failure status {}",
                response.status
            ));
        }
        if response.scores.len() != candidates.len() {
            return Err(
                "worker response score count does not match the candidate snapshot".to_owned(),
            );
        }
        if response.scores.iter().any(|(fingerprint, score)| {
            !score.is_finite()
                || !candidates
                    .iter()
                    .any(|candidate| candidate.fingerprint == *fingerprint)
        }) {
            return Err("worker response has invalid candidate fingerprints or scores".to_owned());
        }
        Ok(response.scores)
    }
}

impl Drop for WorkerClient {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("neural-eval: {error}");
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
    if !options.exploratory {
        validate_acceptance_corpus(&cases).map_err(|reason| {
            format!(
                "{} is not a quality-acceptance corpus: {reason}. Use --exploratory only for a smoke measurement",
                options.corpus.display(),
            )
        })?;
    }
    let report = evaluate(&dictionary, &cases, &options)?;
    write_report(&options.report, &report)?;
    println!(
        "neural-eval {:?}: baseline {}/{}, neural {}/{} (applied {}, fallback {}), candidate recall@6 {}/{}, MRR@6 {:.3}, wins/losses/ties {}/{}/{}",
        report.mode,
        report.baseline.correct,
        report.baseline.evaluated,
        report.neural.correct,
        report.neural.evaluated,
        report.neural.applied,
        report.neural.fallback,
        report.candidate_rank.recall_at_6,
        report.candidate_rank.evaluated,
        mrr(report.candidate_rank.mrr_milli_sum, report.candidate_rank.evaluated),
        report.comparisons.wins,
        report.comparisons.losses,
        report.comparisons.ties,
    );
    println!("report: {}", options.report.display());
    Ok(())
}

fn parse_options(args: impl Iterator<Item = std::ffi::OsString>) -> Result<Options, String> {
    let mut dictionary = None;
    let mut corpus = None;
    let mut worker = None;
    let mut model_directory = None;
    let mut report = None;
    let mut mode = None;
    let mut timeout_ms = None;
    let mut exploratory = false;
    let mut args = args.peekable();
    while let Some(argument) = args.next() {
        let Some(name) = argument.to_str() else {
            return Err("arguments must be valid Unicode".to_owned());
        };
        if matches!(name, "--help" | "-h") {
            println!("Usage: neural-eval --dictionary FILE --corpus FILE --worker FILE --model-dir DIR --mode long|all-normal --report FILE [--timeout-ms 500] [--exploratory]");
            std::process::exit(0);
        }
        if name == "--exploratory" {
            if exploratory {
                return Err("--exploratory was specified more than once".to_owned());
            }
            exploratory = true;
            continue;
        }
        let value = args
            .next()
            .ok_or_else(|| format!("{name} requires a value"))?
            .into_string()
            .map_err(|_| format!("{name} value must be valid Unicode"))?;
        match name {
            "--dictionary" => set_once(&mut dictionary, value, name)?,
            "--corpus" => set_once(&mut corpus, value, name)?,
            "--worker" => set_once(&mut worker, value, name)?,
            "--model-dir" => set_once(&mut model_directory, value, name)?,
            "--report" => set_once(&mut report, value, name)?,
            "--mode" => set_once(&mut mode, value, name)?,
            "--timeout-ms" => set_once(&mut timeout_ms, value, name)?,
            _ => return Err(format!("unknown argument '{name}'")),
        }
    }
    let timeout_ms = timeout_ms
        .unwrap_or_else(|| "500".to_owned())
        .parse::<u64>()
        .map_err(|_| "--timeout-ms must be an integer".to_owned())?;
    if timeout_ms == 0 || timeout_ms > 10_000 {
        return Err("--timeout-ms must be in 1..=10000".to_owned());
    }
    Ok(Options {
        dictionary: PathBuf::from(dictionary.ok_or("--dictionary is required")?),
        corpus: PathBuf::from(corpus.ok_or("--corpus is required")?),
        worker: PathBuf::from(worker.ok_or("--worker is required")?),
        model_directory: PathBuf::from(model_directory.ok_or("--model-dir is required")?),
        report: PathBuf::from(report.ok_or("--report is required")?),
        mode: Mode::parse(&mode.ok_or("--mode is required")?)
            .ok_or("--mode must be long or all-normal")?,
        timeout: Duration::from_millis(timeout_ms),
        exploratory,
    })
}

fn validate_acceptance_corpus(cases: &[Case]) -> Result<(), String> {
    if cases.len() < MINIMUM_ACCEPTANCE_CASES {
        return Err(format!(
            "{} cases; requires at least {MINIMUM_ACCEPTANCE_CASES}",
            cases.len()
        ));
    }
    let counts = slice_counts(cases);
    for (slice, minimum) in [("chat", MINIMUM_CHAT_CASES), ("email", MINIMUM_EMAIL_CASES)] {
        let actual = counts.get(slice).copied().unwrap_or_default();
        if actual < minimum {
            return Err(format!(
                "{slice} slice has {actual} cases; requires at least {minimum}"
            ));
        }
    }
    Ok(())
}

fn acceptance_eligible(cases: &[Case], exploratory: bool) -> bool {
    !exploratory && validate_acceptance_corpus(cases).is_ok()
}

fn slice_counts(cases: &[Case]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for case in cases {
        *counts.entry(case.slice.clone()).or_default() += 1;
    }
    counts
}

fn set_once(slot: &mut Option<String>, value: String, name: &str) -> Result<(), String> {
    if slot.replace(value).is_some() {
        return Err(format!("{name} was specified more than once"));
    }
    Ok(())
}

fn parse_corpus(path: &Path) -> Result<Vec<Case>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    let mut ids = BTreeSet::new();
    let mut cases = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') || line == "id\tslice\treading\texpected" {
            continue;
        }
        let fields: Vec<_> = line.split('\t').collect();
        if fields.len() != 4 || fields.iter().any(|field| field.is_empty()) {
            return Err(format!(
                "{}:{}: expected four non-empty TSV fields",
                path.display(),
                index + 1
            ));
        }
        if !ids.insert(fields[0].to_owned()) {
            return Err(format!(
                "{}:{}: duplicate id '{}'",
                path.display(),
                index + 1,
                fields[0]
            ));
        }
        cases.push(Case {
            id: fields[0].to_owned(),
            slice: fields[1].to_owned(),
            reading: fields[2].to_owned(),
            expected: fields[3].to_owned(),
        });
    }
    if cases.is_empty() {
        return Err(format!("{} contains no cases", path.display()));
    }
    Ok(cases)
}

fn evaluate(
    dictionary: &Dictionary<'_>,
    cases: &[Case],
    options: &Options,
) -> Result<Report, String> {
    let mut converter = Converter::new();
    let mut client = None;
    let mut request_id = 1u64;
    let mut baseline = Top1Score::default();
    let mut neural = NeuralScore::default();
    let mut comparisons = ComparisonScore::default();
    let mut candidate_rank = CandidateRankScore::default();
    let mut slice_metrics = BTreeMap::<String, SliceScore>::new();
    let mut rows = Vec::with_capacity(cases.len());
    for case in cases {
        let snapshot = converter
            .convert(
                dictionary,
                &case.reading,
                ConversionOptions {
                    max_candidates: MAX_CANDIDATES,
                    ..ConversionOptions::default()
                },
            )
            .map_err(|error| format!("{}: conversion failed: {error}", case.id))?;
        let candidates = snapshot_candidates(snapshot);
        let baseline_top1 = candidates
            .first()
            .ok_or_else(|| format!("{}: conversion returned no candidates", case.id))?
            .text
            .clone();
        candidate_rank.observe(&candidates, &case.expected);
        let slice = slice_metrics.entry(case.slice.clone()).or_default();
        slice.cases += 1;
        if candidates
            .iter()
            .any(|candidate| candidate.text == case.expected)
        {
            slice.candidate_recall_at_6 += 1;
            if let Some(rank) = candidates
                .iter()
                .position(|candidate| candidate.text == case.expected)
            {
                slice.candidate_mrr_milli_sum += 1_000 / (rank as u64 + 1);
            }
        }
        let baseline_correct = baseline_top1 == case.expected;
        baseline.evaluated += 1;
        baseline.correct += usize::from(baseline_correct);
        slice.baseline_correct += usize::from(baseline_correct);
        let eligibility = eligibility(options.mode, &case.reading, snapshot);
        let (neural_status, worker_error, neural_top1) = if let Some(reason) = eligibility {
            (reason, None, baseline_top1.clone())
        } else {
            neural.eligible += 1;
            neural.attempted += 1;
            if client.is_none() {
                client = WorkerClient::start(&options.worker, &options.model_directory).ok();
            }
            let scored = client
                .as_mut()
                .ok_or_else(|| "worker did not start".to_owned())
                .and_then(|worker| worker.score(request_id, &candidates, options.timeout));
            request_id = request_id.wrapping_add(1);
            match scored.and_then(|scores| select_top1(&candidates, &scores)) {
                Ok(index) => {
                    neural.applied += 1;
                    ("applied", None, candidates[index].text.clone())
                }
                Err(error) => {
                    client = None;
                    neural.fallback += 1;
                    ("worker-fallback", Some(error), baseline_top1.clone())
                }
            }
        };
        let neural_correct = neural_top1 == case.expected;
        neural.evaluated += 1;
        neural.correct += usize::from(neural_correct);
        slice.neural_correct += usize::from(neural_correct);
        if eligibility.is_none() {
            slice.eligible += 1;
        }
        match neural_status {
            "applied" => slice.applied += 1,
            "worker-fallback" => slice.fallback += 1,
            _ => {}
        }
        let comparison = match (baseline_correct, neural_correct) {
            (false, true) => {
                comparisons.wins += 1;
                slice.wins += 1;
                "win"
            }
            (true, false) => {
                comparisons.losses += 1;
                slice.losses += 1;
                "loss"
            }
            _ => {
                comparisons.ties += 1;
                slice.ties += 1;
                "tie"
            }
        };
        rows.push(Row {
            id: case.id.clone(),
            slice: case.slice.clone(),
            reading: case.reading.clone(),
            expected: case.expected.clone(),
            baseline_top1,
            baseline_correct,
            eligible: eligibility.is_none(),
            eligibility: eligibility.unwrap_or("eligible"),
            neural_status,
            worker_error,
            neural_top1,
            neural_correct,
            comparison,
        });
    }
    Ok(Report {
        schema_version: 2,
        evaluator: "dictc/neural-eval",
        mode: options.mode,
        corpus_contract: "static, explicitly supplied Normal-scope corpus; no live input, history, or user dictionary",
        cases: cases.len(),
        minimum_acceptance_cases: MINIMUM_ACCEPTANCE_CASES,
        minimum_chat_cases: MINIMUM_CHAT_CASES,
        minimum_email_cases: MINIMUM_EMAIL_CASES,
        acceptance_eligible: acceptance_eligible(cases, options.exploratory),
        exploratory: options.exploratory,
        slice_counts: slice_counts(cases),
        baseline,
        neural,
        comparisons,
        candidate_rank,
        slice_metrics,
        rows,
    })
}

fn mrr(milli_sum: u64, evaluated: usize) -> f64 {
    if evaluated == 0 {
        0.0
    } else {
        milli_sum as f64 / evaluated as f64 / 1_000.0
    }
}

fn snapshot_candidates(candidates: &[ConversionCandidate]) -> Vec<SnapshotCandidate> {
    candidates
        .iter()
        .take(MAX_CANDIDATES)
        .map(|candidate| SnapshotCandidate {
            fingerprint: candidate_fingerprint(candidate.text(), candidate.cost),
            text: candidate.text().to_owned(),
            local_cost: candidate.cost,
        })
        .collect()
}

/// `Some` is the stable fallback status. `long` exactly mirrors the engine's
/// current model-snapshot eligibility; `all-normal` intentionally relaxes only
/// the length/segmentation gate for the Issue #32 experiment.
fn eligibility(
    mode: Mode,
    reading: &str,
    candidates: &[ConversionCandidate],
) -> Option<&'static str> {
    if candidates.len() < 2 {
        return Some("too-few-candidates");
    }
    if mode == Mode::Long
        && reading.chars().count() < MINIMUM_LONG_READING_CHARS
        && (reading.chars().count() < MINIMUM_SEGMENTED_READING_CHARS
            || candidates[0].segments().len() < 3)
    {
        return Some("short-or-unsegmented");
    }
    None
}

fn select_top1(candidates: &[SnapshotCandidate], scores: &[(u64, f32)]) -> Result<usize, String> {
    if candidates.len() < 2 || scores.len() != candidates.len() {
        return Err("score count does not match candidates".to_owned());
    }
    let maximum = scores
        .iter()
        .map(|(_, score)| *score)
        .reduce(f32::max)
        .filter(|score| score.is_finite())
        .ok_or("no finite score")?;
    let mut selected = 0usize;
    let mut selected_cost = i64::MAX;
    for (index, candidate) in candidates.iter().enumerate() {
        let score = scores
            .iter()
            .find_map(|(fingerprint, score)| {
                (*fingerprint == candidate.fingerprint).then_some(*score)
            })
            .filter(|score| score.is_finite())
            .ok_or("missing finite candidate score")?;
        let penalty = ((maximum - score) * MODEL_PENALTY_PER_NAT)
            .round()
            .clamp(0.0, MAXIMUM_MODEL_PENALTY as f32) as i64;
        let combined = candidate.local_cost.saturating_add(penalty);
        if combined < selected_cost {
            selected = index;
            selected_cost = combined;
        }
    }
    Ok(selected)
}

fn encode_request(request_id: u64, candidates: &[SnapshotCandidate]) -> Result<Vec<u8>, String> {
    if candidates.is_empty() || candidates.len() > MAX_CANDIDATES {
        return Err("candidate count is out of bounds".to_owned());
    }
    let mut payload = Vec::new();
    put_u32(&mut payload, REQUEST_MAGIC);
    put_u16(&mut payload, PROTOCOL_VERSION);
    put_u16(&mut payload, 0);
    put_u64(&mut payload, request_id);
    put_u32(&mut payload, 0);
    put_u32(&mut payload, candidates.len() as u32);
    for candidate in candidates {
        if candidate.text.is_empty() || candidate.text.len() > 3 * 1024 {
            return Err("candidate text is out of bounds".to_owned());
        }
        put_u64(&mut payload, candidate.fingerprint);
        put_i32(
            &mut payload,
            candidate.local_cost.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        );
        put_u32(&mut payload, candidate.text.len() as u32);
        payload.extend_from_slice(candidate.text.as_bytes());
    }
    if payload.len() > MAX_FRAME_BYTES {
        return Err("request frame is too large".to_owned());
    }
    let mut frame = Vec::with_capacity(payload.len() + 4);
    put_u32(&mut frame, payload.len() as u32);
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn read_response(input: &mut impl Read) -> io::Result<WorkerResponse> {
    let mut length = [0u8; 4];
    input.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid response length",
        ));
    }
    let mut payload = vec![0; length];
    input.read_exact(&mut payload)?;
    let mut cursor = 0;
    if take_u32(&payload, &mut cursor)? != RESPONSE_MAGIC
        || take_u16(&payload, &mut cursor)? != PROTOCOL_VERSION
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid response header",
        ));
    }
    let status = take_u16(&payload, &mut cursor)?;
    let request_id = take_u64(&payload, &mut cursor)?;
    let _tier = take_u16(&payload, &mut cursor)?;
    if take_u16(&payload, &mut cursor)? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid response reserved field",
        ));
    }
    let count = take_u32(&payload, &mut cursor)? as usize;
    if count > MAX_CANDIDATES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid response count",
        ));
    }
    let mut scores = Vec::with_capacity(count);
    for _ in 0..count {
        scores.push((
            take_u64(&payload, &mut cursor)?,
            f32::from_bits(take_u32(&payload, &mut cursor)?),
        ));
    }
    if cursor != payload.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "trailing response bytes",
        ));
    }
    Ok(WorkerResponse {
        request_id,
        status,
        scores,
    })
}

fn candidate_fingerprint(text: &str, cost: i64) -> u64 {
    hash_bytes(
        hash_bytes(0xCBF2_9CE4_8422_2325, text.as_bytes()),
        &cost.to_le_bytes(),
    )
}
fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}
fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes())
}
fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes())
}
fn put_i32(out: &mut Vec<u8>, value: i32) {
    out.extend_from_slice(&value.to_le_bytes())
}
fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes())
}
fn take<const N: usize>(input: &[u8], cursor: &mut usize) -> io::Result<[u8; N]> {
    let end = cursor
        .checked_add(N)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "response overflow"))?;
    let bytes = input
        .get(*cursor..end)
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "truncated response"))?;
    *cursor = end;
    Ok(bytes.try_into().expect("slice length is fixed"))
}
fn take_u16(input: &[u8], cursor: &mut usize) -> io::Result<u16> {
    Ok(u16::from_le_bytes(take(input, cursor)?))
}
fn take_u32(input: &[u8], cursor: &mut usize) -> io::Result<u32> {
    Ok(u32::from_le_bytes(take(input, cursor)?))
}
fn take_u64(input: &[u8], cursor: &mut usize) -> io::Result<u64> {
    Ok(u64::from_le_bytes(take(input, cursor)?))
}

fn write_report(path: &Path, report: &Report) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    let json =
        serde_json::to_vec_pretty(report).map_err(|error| format!("serialize report: {error}"))?;
    std::fs::write(path, json).map_err(|error| format!("write {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(fingerprint: u64, text: &str, local_cost: i64) -> SnapshotCandidate {
        SnapshotCandidate {
            fingerprint,
            text: text.to_owned(),
            local_cost,
        }
    }

    #[test]
    fn selection_uses_the_engine_penalty_formula() {
        let candidates = vec![candidate(1, "local", 100), candidate(2, "model", 500)];
        assert_eq!(
            select_top1(&candidates, &[(1, -10.0), (2, 0.0)]).unwrap(),
            1
        );
        assert_eq!(
            select_top1(&candidates, &[(1, 0.0), (2, -10.0)]).unwrap(),
            0
        );
    }

    #[test]
    fn request_is_bounded_and_has_worker_wire_header() {
        let frame = encode_request(9, &[candidate(7, "候補", 12)]).unwrap();
        assert_eq!(
            u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize,
            frame.len() - 4
        );
        let mut cursor = 4;
        assert_eq!(take_u32(&frame, &mut cursor).unwrap(), REQUEST_MAGIC);
        assert_eq!(take_u16(&frame, &mut cursor).unwrap(), PROTOCOL_VERSION);
        assert!(encode_request(1, &[]).is_err());
    }

    #[test]
    fn response_parser_rejects_trailing_data() {
        let mut bytes = Vec::new();
        let mut payload = Vec::new();
        put_u32(&mut payload, RESPONSE_MAGIC);
        put_u16(&mut payload, PROTOCOL_VERSION);
        put_u16(&mut payload, 0);
        put_u64(&mut payload, 1);
        put_u16(&mut payload, 1);
        put_u16(&mut payload, 0);
        put_u32(&mut payload, 0);
        payload.push(0);
        put_u32(&mut bytes, payload.len() as u32);
        bytes.extend_from_slice(&payload);
        assert!(read_response(&mut bytes.as_slice()).is_err());
    }

    #[test]
    fn fixture_scores_are_test_only_not_human_corpus_answers() {
        // Synthetic candidate texts only exercise the evaluator's comparison
        // rules. They are not a conversion corpus and make no quality claim.
        let baseline_correct = false;
        let neural_correct = true;
        assert_eq!(
            match (baseline_correct, neural_correct) {
                (false, true) => "win",
                (true, false) => "loss",
                _ => "tie",
            },
            "win"
        );
    }

    #[test]
    fn candidate_rank_metrics_count_recall_and_first_match_mrr() {
        let candidates = vec![
            candidate(1, "別解", 10),
            candidate(2, "正解", 20),
            candidate(3, "別候補", 30),
        ];
        let mut metrics = CandidateRankScore::default();
        metrics.observe(&candidates, "正解");
        metrics.observe(&candidates, "存在しない");
        assert_eq!(metrics.evaluated, 2);
        assert_eq!(metrics.recall_at_6, 1);
        assert_eq!(metrics.mrr_milli_sum, 500);
    }

    #[test]
    fn quality_acceptance_requires_six_hundred_cases_and_chat_email_coverage() {
        let sparse = [Case {
            id: "case-1".to_owned(),
            slice: "general".to_owned(),
            reading: "かくにん".to_owned(),
            expected: "確認".to_owned(),
        }];
        assert!(validate_acceptance_corpus(&sparse).is_err());

        let mut cases = Vec::new();
        for index in 0..MINIMUM_ACCEPTANCE_CASES {
            let slice = if index < MINIMUM_CHAT_CASES {
                "chat"
            } else if index < MINIMUM_CHAT_CASES + MINIMUM_EMAIL_CASES {
                "email"
            } else {
                "general"
            };
            cases.push(Case {
                id: format!("case-{index}"),
                slice: slice.to_owned(),
                reading: "かくにん".to_owned(),
                expected: "確認".to_owned(),
            });
        }
        assert!(validate_acceptance_corpus(&cases).is_ok());
        assert!(acceptance_eligible(&cases, false));
        assert!(!acceptance_eligible(&cases, true));
        assert_eq!(slice_counts(&cases).get("chat"), Some(&MINIMUM_CHAT_CASES));
        assert_eq!(
            slice_counts(&cases).get("email"),
            Some(&MINIMUM_EMAIL_CASES)
        );
    }
}
