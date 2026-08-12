//! Optional long-reading reranking through the isolated ONNX worker.
//!
//! The key path only replaces a one-slot request. Dictionary conversion and
//! model inference run on this module's single worker thread. Conversion never
//! waits for it: only an exact owner/session/generation/reading/candidate-set
//! result can be consumed, otherwise the existing local ranking remains final.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use sakura_core::{ConversionCandidate, ConversionOptions};
use sakura_proto::SessionId;

use crate::dictionary::ConversionService;

const MINIMUM_LONG_READING_CHARS: usize = 10;
const MINIMUM_SEGMENTED_READING_CHARS: usize = 3;
const MAXIMUM_MODEL_CANDIDATES: usize = 6;
const MODEL_PENALTY_PER_NAT: f32 = 240.0;
const MAXIMUM_MODEL_PENALTY: i64 = 1_200;
const WORKER_RESPONSE_TIMEOUT: Duration = Duration::from_millis(500);
const MAXIMUM_FRAME_BYTES: usize = 32 * 1024;
const REQUEST_MAGIC: u32 = 0x524E_4B53; // SKNR
const RESPONSE_MAGIC: u32 = 0x534E_4B53; // SKNS
const PROTOCOL_VERSION: u16 = 1;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RerankState {
    NotEligible,
    Queued,
    Ready,
    Applied,
    LocalFallback,
    TimedOut,
    Stale,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RequestKey {
    owner: u64,
    session: SessionId,
    generation: u64,
    reading_hash: u64,
    reading_bytes: u16,
}

#[derive(Debug, Clone)]
struct WorkRequest {
    key: RequestKey,
    reading: String,
    options: ConversionOptions,
    /// `false` retains the original long-reading / multi-segment gate.  The
    /// all-normal-conversions setting may opt into otherwise short readings,
    /// but never widens the worker's candidate-count or frame-size limits.
    allow_short_reading: bool,
}

#[derive(Debug, Clone)]
struct ModelCandidate {
    fingerprint: u64,
    local_cost: i64,
    text: String,
}

#[derive(Debug, Clone)]
struct CandidateScore {
    fingerprint: u64,
    log_probability: f32,
}

#[derive(Debug, Clone)]
struct WorkResult {
    key: RequestKey,
    candidate_set: u64,
    scores: Vec<CandidateScore>,
    state: RerankState,
}

#[derive(Debug)]
struct Shared {
    conversion: Arc<ConversionService>,
    worker_path: PathBuf,
    model_directory: PathBuf,
    pending: Mutex<Option<WorkRequest>>,
    ready: Condvar,
    result: Mutex<Option<WorkResult>>,
    shutdown: AtomicBool,
    next_owner: AtomicU64,
    next_request: AtomicU64,
}

/// Nonblocking request/result side shared by all pipe dispatchers.
#[derive(Debug)]
pub struct LongConversionService {
    shared: Arc<Shared>,
}

impl LongConversionService {
    pub fn allocate_owner(&self) -> u64 {
        self.shared.next_owner.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn schedule(
        &self,
        owner: u64,
        session: SessionId,
        generation: u64,
        reading: &str,
        options: ConversionOptions,
        allow_short_reading: bool,
    ) -> RerankState {
        if !reading_is_eligible(reading, allow_short_reading) || reading.len() > u16::MAX as usize {
            return RerankState::NotEligible;
        }
        let request = WorkRequest {
            key: RequestKey {
                owner,
                session,
                generation,
                reading_hash: fingerprint_bytes(reading.as_bytes()),
                reading_bytes: reading.len() as u16,
            },
            reading: reading.to_owned(),
            options,
            allow_short_reading,
        };
        *lock(&self.shared.pending) = Some(request);
        self.shared.ready.notify_one();
        RerankState::Queued
    }

    /// Returns a model-selected candidate only for the exact snapshot. The
    /// caller owns the authoritative learning/cache precedence check.
    pub(crate) fn selection(
        &self,
        owner: u64,
        session: SessionId,
        generation: u64,
        reading: &str,
        candidates: &[ConversionCandidate],
    ) -> Option<usize> {
        let expected_key = RequestKey {
            owner,
            session,
            generation,
            reading_hash: fingerprint_bytes(reading.as_bytes()),
            reading_bytes: u16::try_from(reading.len()).ok()?,
        };
        let mut result = lock(&self.shared.result);
        let result = result.as_mut()?;
        if result.state != RerankState::Ready || result.key != expected_key {
            return None;
        }
        let model_count = candidates.len().min(MAXIMUM_MODEL_CANDIDATES);
        if model_count < 2 || result.scores.len() != model_count {
            return None;
        }
        if result.candidate_set != candidate_set_fingerprint(&candidates[..model_count]) {
            return None;
        }

        let maximum_score = result
            .scores
            .iter()
            .map(|score| score.log_probability)
            .reduce(f32::max)?;
        if !maximum_score.is_finite() {
            return None;
        }

        let mut selected = 0usize;
        let mut selected_cost = i64::MAX;
        for (index, candidate) in candidates[..model_count].iter().enumerate() {
            let expected_fingerprint = candidate_fingerprint(candidate);
            let score = result
                .scores
                .iter()
                .find(|score| score.fingerprint == expected_fingerprint)?;
            if !score.log_probability.is_finite() {
                return None;
            }
            let penalty = ((maximum_score - score.log_probability) * MODEL_PENALTY_PER_NAT)
                .round()
                .clamp(0.0, MAXIMUM_MODEL_PENALTY as f32) as i64;
            let combined = candidate.cost.saturating_add(penalty);
            if combined < selected_cost {
                selected = index;
                selected_cost = combined;
            }
        }
        result.state = RerankState::Applied;
        Some(selected)
    }
}

/// Owns the bounded worker thread and its exact child-process cleanup.
#[derive(Debug)]
pub struct LongConversionRuntime {
    service: Arc<LongConversionService>,
    thread: Option<JoinHandle<()>>,
}

impl LongConversionRuntime {
    pub fn discover(conversion: Arc<ConversionService>) -> io::Result<Option<Self>> {
        let executable = std::env::current_exe()?;
        let Some(directory) = executable.parent() else {
            return Ok(None);
        };
        let worker = directory.join("sakura_neural_worker.exe");
        let model = directory
            .join("neural")
            .join("deberta-v2-tiny-japanese-char-wwm");
        if !worker.is_file()
            || !model.join("model.onnx").is_file()
            || !model.join("vocab.txt").is_file()
            || !model.join("manifest.json").is_file()
        {
            return Ok(None);
        }
        Self::start(conversion, worker, model).map(Some)
    }

    pub fn start(
        conversion: Arc<ConversionService>,
        worker_path: PathBuf,
        model_directory: PathBuf,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            conversion,
            worker_path,
            model_directory,
            pending: Mutex::new(None),
            ready: Condvar::new(),
            result: Mutex::new(None),
            shutdown: AtomicBool::new(false),
            next_owner: AtomicU64::new(1),
            next_request: AtomicU64::new(1),
        });
        let worker_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("sakura-long-conversion".to_owned())
            .spawn(move || worker_loop(worker_shared))?;
        Ok(Self {
            service: Arc::new(LongConversionService { shared }),
            thread: Some(thread),
        })
    }

    pub fn service(&self) -> Arc<LongConversionService> {
        Arc::clone(&self.service)
    }
}

impl Drop for LongConversionRuntime {
    fn drop(&mut self) {
        self.service.shared.shutdown.store(true, Ordering::Release);
        self.service.shared.ready.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn worker_loop(shared: Arc<Shared>) {
    let mut process: Option<ProcessClient> = None;
    let mut failures = 0u32;
    let mut retry_after = Instant::now();
    loop {
        let work = {
            let mut pending = lock(&shared.pending);
            while pending.is_none() && !shared.shutdown.load(Ordering::Acquire) {
                pending = shared
                    .ready
                    .wait(pending)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }
            if shared.shutdown.load(Ordering::Acquire) {
                break;
            }
            pending.take()
        };
        let Some(work) = work else { continue };

        let Some(candidates) = build_candidates(&shared.conversion, &work) else {
            publish_terminal(&shared, &work.key, RerankState::LocalFallback);
            continue;
        };
        if Instant::now() < retry_after {
            publish_terminal(&shared, &work.key, RerankState::Failed);
            continue;
        }

        if process.is_none() {
            match ProcessClient::start(&shared.worker_path, &shared.model_directory) {
                Ok(started) => process = Some(started),
                Err(_) => {
                    failures = failures.saturating_add(1);
                    retry_after = Instant::now() + retry_delay(failures, work.key.generation);
                    publish_terminal(&shared, &work.key, RerankState::Failed);
                    continue;
                }
            }
        }

        let request_id = shared.next_request.fetch_add(1, Ordering::Relaxed);
        let response = process
            .as_mut()
            .expect("process was initialized")
            .score(request_id, &candidates);
        match response {
            Ok(scores) if scores.len() == candidates.len() => {
                failures = 0;
                retry_after = Instant::now();
                *lock(&shared.result) = Some(WorkResult {
                    key: work.key,
                    candidate_set: model_candidate_set_fingerprint(&candidates),
                    scores,
                    state: RerankState::Ready,
                });
            }
            Ok(_) => {
                process = None;
                failures = failures.saturating_add(1);
                retry_after = Instant::now() + retry_delay(failures, work.key.generation);
                publish_terminal(&shared, &work.key, RerankState::Failed);
            }
            Err(state) => {
                process = None;
                failures = failures.saturating_add(1);
                retry_after = Instant::now() + retry_delay(failures, work.key.generation);
                publish_terminal(&shared, &work.key, state);
            }
        }
    }
    drop(process);
}

fn build_candidates(
    conversion: &ConversionService,
    work: &WorkRequest,
) -> Option<Vec<ModelCandidate>> {
    let mut options = work.options;
    options.max_candidates = MAXIMUM_MODEL_CANDIDATES;
    conversion
        .with_candidates(&work.reading, options, |candidates| {
            if candidates.len() < 2
                || (!work.allow_short_reading
                    && (work.reading.chars().count() < MINIMUM_LONG_READING_CHARS
                        && candidates
                            .first()
                            .map_or(0, |candidate| candidate.segments().len())
                            < 3))
            {
                return None;
            }
            Some(
                candidates
                    .iter()
                    .take(MAXIMUM_MODEL_CANDIDATES)
                    .map(|candidate| ModelCandidate {
                        fingerprint: candidate_fingerprint(candidate),
                        local_cost: candidate.cost,
                        text: candidate.text().to_owned(),
                    })
                    .collect(),
            )
        })
        .ok()
        .flatten()
}

fn reading_can_have_three_segments(reading: &str) -> bool {
    reading.chars().count() >= MINIMUM_SEGMENTED_READING_CHARS
}

fn reading_is_eligible(reading: &str, allow_short_reading: bool) -> bool {
    !reading.is_empty() && (allow_short_reading || reading_can_have_three_segments(reading))
}

fn publish_terminal(shared: &Shared, key: &RequestKey, state: RerankState) {
    *lock(&shared.result) = Some(WorkResult {
        key: key.clone(),
        candidate_set: 0,
        scores: Vec::new(),
        state,
    });
}

fn retry_delay(failures: u32, salt: u64) -> Duration {
    let exponent = failures.saturating_sub(1).min(5);
    let base_ms = 250u64.saturating_mul(1u64 << exponent);
    let jitter = (salt.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 58) * 7;
    Duration::from_millis((base_ms + jitter).min(8_000))
}

struct ProcessClient {
    child: Child,
    input: ChildStdin,
    responses: mpsc::Receiver<Result<WorkerResponse, String>>,
    reader: Option<JoinHandle<()>>,
}

impl ProcessClient {
    fn start(worker_path: &Path, model_directory: &Path) -> io::Result<Self> {
        use std::os::windows::process::CommandExt;

        let mut child = Command::new(worker_path)
            .arg("--stdio")
            .arg("--model-dir")
            .arg(model_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()?;
        let Some(input) = child.stdin.take() else {
            terminate_child(&mut child);
            return Err(io::Error::other("neural worker stdin was not piped"));
        };
        let Some(mut output) = child.stdout.take() else {
            terminate_child(&mut child);
            return Err(io::Error::other("neural worker stdout was not piped"));
        };
        let (send, responses) = mpsc::channel();
        let reader = match thread::Builder::new()
            .name("sakura-neural-response".to_owned())
            .spawn(move || loop {
                let response = read_worker_response(&mut output).map_err(|error| error.to_string());
                let terminal = response.is_err();
                if send.send(response).is_err() || terminal {
                    break;
                }
            }) {
            Ok(reader) => reader,
            Err(error) => {
                terminate_child(&mut child);
                return Err(error);
            }
        };
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
        candidates: &[ModelCandidate],
    ) -> Result<Vec<CandidateScore>, RerankState> {
        let frame = encode_request(request_id, candidates).map_err(|_| RerankState::Failed)?;
        self.input
            .write_all(&frame)
            .map_err(|_| RerankState::Failed)?;
        self.input.flush().map_err(|_| RerankState::Failed)?;
        let response = match self.responses.recv_timeout(WORKER_RESPONSE_TIMEOUT) {
            Ok(response) => response.map_err(|_| RerankState::Failed)?,
            Err(mpsc::RecvTimeoutError::Timeout) => return Err(RerankState::TimedOut),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(RerankState::Failed),
        };
        if response.request_id != request_id || response.status != 0 {
            return Err(RerankState::Failed);
        }
        Ok(response.scores)
    }
}

impl Drop for ProcessClient {
    fn drop(&mut self) {
        terminate_child(&mut self.child);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn terminate_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

#[derive(Debug)]
struct WorkerResponse {
    request_id: u64,
    status: u16,
    scores: Vec<CandidateScore>,
}

fn encode_request(request_id: u64, candidates: &[ModelCandidate]) -> Result<Vec<u8>, String> {
    if candidates.is_empty() || candidates.len() > MAXIMUM_MODEL_CANDIDATES {
        return Err("neural candidate count is out of bounds".to_owned());
    }
    let mut payload = Vec::with_capacity(128);
    push_u32(&mut payload, REQUEST_MAGIC);
    push_u16(&mut payload, PROTOCOL_VERSION);
    push_u16(&mut payload, 0);
    push_u64(&mut payload, request_id);
    push_u32(&mut payload, 0); // Reserved context bytes.
    push_u32(&mut payload, candidates.len() as u32);
    for candidate in candidates {
        if candidate.text.is_empty() || candidate.text.len() > 3 * 1024 {
            return Err("neural candidate text is out of bounds".to_owned());
        }
        push_u64(&mut payload, candidate.fingerprint);
        push_i32(
            &mut payload,
            candidate.local_cost.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
        );
        push_u32(&mut payload, candidate.text.len() as u32);
        payload.extend_from_slice(candidate.text.as_bytes());
    }
    if payload.len() > MAXIMUM_FRAME_BYTES {
        return Err("neural request frame is too large".to_owned());
    }
    let mut frame = Vec::with_capacity(payload.len() + 4);
    push_u32(&mut frame, payload.len() as u32);
    frame.extend_from_slice(&payload);
    Ok(frame)
}

fn read_worker_response(input: &mut impl Read) -> io::Result<WorkerResponse> {
    let mut length = [0u8; 4];
    input.read_exact(&mut length)?;
    let length = u32::from_le_bytes(length) as usize;
    if length == 0 || length > MAXIMUM_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "neural response length is invalid",
        ));
    }
    let mut payload = vec![0u8; length];
    input.read_exact(&mut payload)?;
    let mut cursor = 0usize;
    if take_u32(&payload, &mut cursor)? != RESPONSE_MAGIC
        || take_u16(&payload, &mut cursor)? != PROTOCOL_VERSION
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "neural response header is invalid",
        ));
    }
    let status = take_u16(&payload, &mut cursor)?;
    let request_id = take_u64(&payload, &mut cursor)?;
    let _cpu_tier = take_u16(&payload, &mut cursor)?;
    if take_u16(&payload, &mut cursor)? != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "neural response reserved field is invalid",
        ));
    }
    let count = take_u32(&payload, &mut cursor)? as usize;
    if count > MAXIMUM_MODEL_CANDIDATES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "neural response count is invalid",
        ));
    }
    let mut scores = Vec::with_capacity(count);
    for _ in 0..count {
        let fingerprint = take_u64(&payload, &mut cursor)?;
        let log_probability = f32::from_bits(take_u32(&payload, &mut cursor)?);
        scores.push(CandidateScore {
            fingerprint,
            log_probability,
        });
    }
    if cursor != payload.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "neural response has trailing bytes",
        ));
    }
    Ok(WorkerResponse {
        request_id,
        status,
        scores,
    })
}

fn candidate_fingerprint(candidate: &ConversionCandidate) -> u64 {
    let mut hash = fingerprint_bytes(candidate.text().as_bytes());
    hash = hash_bytes(hash, &candidate.cost.to_le_bytes());
    hash
}

fn candidate_set_fingerprint(candidates: &[ConversionCandidate]) -> u64 {
    candidates
        .iter()
        .fold(0xCBF2_9CE4_8422_2325, |hash, candidate| {
            hash_bytes(hash, &candidate_fingerprint(candidate).to_le_bytes())
        })
}

fn model_candidate_set_fingerprint(candidates: &[ModelCandidate]) -> u64 {
    candidates
        .iter()
        .fold(0xCBF2_9CE4_8422_2325, |hash, candidate| {
            hash_bytes(hash, &candidate.fingerprint.to_le_bytes())
        })
}

fn fingerprint_bytes(bytes: &[u8]) -> u64 {
    hash_bytes(0xCBF2_9CE4_8422_2325, bytes)
}

fn hash_bytes(mut hash: u64, bytes: &[u8]) -> u64 {
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn push_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_i32(output: &mut Vec<u8>, value: i32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn take_u16(input: &[u8], cursor: &mut usize) -> io::Result<u16> {
    let bytes = take::<2>(input, cursor)?;
    Ok(u16::from_le_bytes(bytes))
}

fn take_u32(input: &[u8], cursor: &mut usize) -> io::Result<u32> {
    let bytes = take::<4>(input, cursor)?;
    Ok(u32::from_le_bytes(bytes))
}

fn take_u64(input: &[u8], cursor: &mut usize) -> io::Result<u64> {
    let bytes = take::<8>(input, cursor)?;
    Ok(u64::from_le_bytes(bytes))
}

fn take<const N: usize>(input: &[u8], cursor: &mut usize) -> io::Result<[u8; N]> {
    let end = cursor
        .checked_add(N)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "neural response overflow"))?;
    let bytes = input.get(*cursor..end).ok_or_else(|| {
        io::Error::new(io::ErrorKind::UnexpectedEof, "neural response is truncated")
    })?;
    *cursor = end;
    Ok(bytes.try_into().expect("slice length is fixed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_is_bounded_and_jittered() {
        assert!(retry_delay(1, 1) >= Duration::from_millis(250));
        assert!(retry_delay(100, 2) <= Duration::from_millis(8_000));
        assert_ne!(retry_delay(2, 1), retry_delay(2, 2));
    }

    #[test]
    fn response_parser_rejects_oversized_frames() {
        let length = ((MAXIMUM_FRAME_BYTES + 1) as u32).to_le_bytes();
        let mut bytes = length.as_slice();
        assert_eq!(
            read_worker_response(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn precompute_skips_readings_that_cannot_have_three_segments() {
        assert!(!reading_can_have_three_segments("かな"));
        assert!(reading_can_have_three_segments("かな文"));
    }

    #[test]
    fn all_normal_scope_keeps_short_readings_eligible_without_widening_empty_input() {
        assert!(!reading_is_eligible("", true));
        assert!(!reading_is_eligible("かな", false));
        assert!(reading_is_eligible("かな", true));
        assert!(reading_is_eligible("かな文", false));
    }
}
