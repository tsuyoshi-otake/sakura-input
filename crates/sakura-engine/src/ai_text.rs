//! Bounded asynchronous ownership of the isolated AI worker process.
//!
//! The TSF DLL starts and polls jobs through the engine pipe; only this
//! out-of-process engine launches children. Every child has one cancellation
//! flag, one 30-second deadline, and one exact kill/wait owner.

use std::io::{self, Read};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sakura_ai_proto::{Auth, Effort, Operation, Provider, Request, ServiceTier, Status, Style};
use sakura_proto::{AiTextOperation, AiTextStatus, SessionId};
use sakura_reg::user_preferences::{self, AiAuth, AiEffort, AiProvider, AiServiceTier, AiStyle};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

const MAX_JOBS: usize = 1;
const WORKER_DEADLINE: Duration = Duration::from_secs(30);
const WAIT_QUANTUM: Duration = Duration::from_millis(25);
const MAX_WORKER_RESPONSE_BYTES: u64 = 16 * 1024;
const DUPLICATE_COOLDOWN: Duration = Duration::from_secs(2);
const MAX_COOLDOWNS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiTextResult {
    pub status: AiTextStatus,
    pub result: String,
    pub model: String,
    pub provider: String,
    pub style: String,
    pub error_code: String,
    pub latency_ms: u64,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cached_tokens: u32,
    pub attempts: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    Pending,
    Complete(AiTextResult),
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartError {
    Duplicate,
    Capacity,
    Invalid,
    Spawn,
}

#[derive(Debug)]
enum JobState {
    Pending,
    Complete(AiTextResult),
}

#[derive(Debug)]
struct Job {
    id: u64,
    owner: u64,
    session: SessionId,
    cancel: Arc<AtomicBool>,
    signature: RequestSignature,
    detached: bool,
    state: JobState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RequestSignature {
    operation: AiTextOperation,
    text_hash: u64,
    text_bytes: usize,
}

#[derive(Debug)]
struct Cooldown {
    signature: RequestSignature,
    until: Instant,
}

#[derive(Debug, Default)]
struct State {
    jobs: Vec<Job>,
    cooldowns: Vec<Cooldown>,
}

#[derive(Debug)]
pub struct AiTextService {
    worker: PathBuf,
    next_id: AtomicU64,
    next_owner: AtomicU64,
    state: Arc<Mutex<State>>,
}

impl Default for AiTextService {
    fn default() -> Self {
        Self::new(default_worker_path())
    }
}

impl AiTextService {
    pub fn new(worker: PathBuf) -> Self {
        Self {
            worker,
            next_id: AtomicU64::new(0),
            next_owner: AtomicU64::new(0),
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    pub fn allocate_owner(&self) -> u64 {
        self.next_owner
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1)
    }

    pub fn start(
        &self,
        owner: u64,
        session: SessionId,
        operation: AiTextOperation,
        text: &str,
    ) -> Result<u64, StartError> {
        if text.is_empty() || text.len() > sakura_ai_proto::MAX_TEXT_BYTES {
            return Err(StartError::Invalid);
        }
        let mut state = lock(&self.state);
        let now = Instant::now();
        state.cooldowns.retain(|cooldown| cooldown.until > now);
        let signature = RequestSignature {
            operation,
            text_hash: text_hash(text.as_bytes()),
            text_bytes: text.len(),
        };
        if state
            .cooldowns
            .iter()
            .any(|cooldown| cooldown.signature == signature)
        {
            return Err(StartError::Duplicate);
        }
        if state
            .jobs
            .iter()
            .any(|job| job.owner == owner && job.session == session)
        {
            return Err(StartError::Duplicate);
        }
        if state.jobs.len() >= MAX_JOBS {
            return Err(StartError::Capacity);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let cancel = Arc::new(AtomicBool::new(false));
        state.jobs.push(Job {
            id,
            owner,
            session,
            cancel: Arc::clone(&cancel),
            signature,
            detached: false,
            state: JobState::Pending,
        });
        drop(state);

        let worker = self.worker.clone();
        let shared = Arc::clone(&self.state);
        let source = text.to_owned();
        let spawn = thread::Builder::new()
            .name("sakura-ai-job".to_owned())
            .spawn(move || {
                let request = Request {
                    id,
                    operation: match operation {
                        AiTextOperation::Transform => Operation::Transform,
                        AiTextOperation::Proofread => Operation::Proofread,
                    },
                    provider: map_provider(user_preferences::read_ai_text_preferences().provider),
                    endpoint: String::new(),
                    auth: Auth::Bearer,
                    api_key: String::new(),
                    style: Style::Polite,
                    effort: Effort::Low,
                    service_tier: ServiceTier::ProviderDefault,
                    text: source,
                };
                let result = run_worker_with_preferences(&worker, request, &cancel);
                let mut state = lock(&shared);
                if let Some(index) = state
                    .jobs
                    .iter()
                    .position(|job| job.id == id && job.owner == owner && job.session == session)
                {
                    let signature = state.jobs[index].signature;
                    state
                        .cooldowns
                        .retain(|cooldown| cooldown.until > Instant::now());
                    if state.cooldowns.len() >= MAX_COOLDOWNS {
                        state.cooldowns.remove(0);
                    }
                    state.cooldowns.push(Cooldown {
                        signature,
                        until: Instant::now() + DUPLICATE_COOLDOWN,
                    });
                    if state.jobs[index].detached {
                        state.jobs.swap_remove(index);
                    } else {
                        state.jobs[index].state = JobState::Complete(result);
                    }
                }
            });
        if spawn.is_err() {
            let mut state = lock(&self.state);
            state
                .jobs
                .retain(|job| job.id != id || job.owner != owner || job.session != session);
            return Err(StartError::Spawn);
        }
        Ok(id)
    }

    pub fn poll(&self, owner: u64, session: SessionId, id: u64) -> Poll {
        let mut state = lock(&self.state);
        let Some(index) = state
            .jobs
            .iter()
            .position(|job| job.id == id && job.owner == owner && job.session == session)
        else {
            return Poll::Missing;
        };
        if state.jobs[index].detached {
            return Poll::Missing;
        }
        if matches!(state.jobs[index].state, JobState::Pending) {
            return Poll::Pending;
        }
        let job = state.jobs.swap_remove(index);
        match job.state {
            JobState::Complete(result) => Poll::Complete(result),
            JobState::Pending => Poll::Missing,
        }
    }

    pub fn cancel(&self, owner: u64, session: SessionId, id: u64) -> bool {
        let mut state = lock(&self.state);
        let Some(index) = state
            .jobs
            .iter()
            .position(|job| job.id == id && job.owner == owner && job.session == session)
        else {
            return false;
        };
        if state.jobs[index].detached {
            return false;
        }
        state.jobs[index].cancel.store(true, Ordering::Release);
        state.jobs[index].detached = true;
        if matches!(state.jobs[index].state, JobState::Complete(_)) {
            state.jobs.swap_remove(index);
        }
        true
    }

    pub fn cancel_owner(&self, owner: u64) {
        let mut state = lock(&self.state);
        for job in state.jobs.iter_mut().filter(|job| job.owner == owner) {
            job.cancel.store(true, Ordering::Release);
            job.detached = true;
        }
        state
            .jobs
            .retain(|job| job.owner != owner || matches!(job.state, JobState::Pending));
    }

    pub fn cancel_session(&self, owner: u64, session: SessionId) {
        let mut state = lock(&self.state);
        for job in state
            .jobs
            .iter_mut()
            .filter(|job| job.owner == owner && job.session == session)
        {
            job.cancel.store(true, Ordering::Release);
            job.detached = true;
        }
        state.jobs.retain(|job| {
            job.owner != owner || job.session != session || matches!(job.state, JobState::Pending)
        });
    }
}

fn run_worker_with_preferences(
    worker: &Path,
    mut request: Request,
    cancel: &AtomicBool,
) -> AiTextResult {
    let preferences = user_preferences::read_ai_text_preferences();
    request.provider = map_provider(preferences.provider);
    request.endpoint = preferences.endpoint;
    request.auth = map_auth(preferences.auth);
    request.api_key = if preferences.auth == AiAuth::None {
        String::new()
    } else {
        match user_preferences::read_api_key() {
            Ok(value) => value.unwrap_or_default(),
            Err(_) => {
                let mut result = terminal(AiTextStatus::WorkerError, "credential_read");
                result.provider = provider_name(preferences.provider).to_owned();
                result.style = style_name(preferences.style).to_owned();
                return result;
            }
        }
    };
    request.style = map_style(preferences.style);
    request.effort = map_effort(preferences.effort);
    request.service_tier = map_service_tier(preferences.service_tier);
    let mut result = run_worker(worker, &request, cancel);
    // SAFETY: zero is valid UTF-8, and the credential is not observed again.
    unsafe { request.api_key.as_bytes_mut() }.fill(0);
    request.api_key.clear();
    result.provider = provider_name(preferences.provider).to_owned();
    result.style = style_name(preferences.style).to_owned();
    result
}

fn run_worker(worker: &Path, request: &Request, cancel: &AtomicBool) -> AiTextResult {
    let mut child = match Command::new(worker)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW.0)
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return terminal(AiTextStatus::WorkerError, "worker_spawn"),
    };
    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return terminal(AiTextStatus::WorkerError, "worker_stdout");
    };
    // Read concurrently: waiting for process exit before draining a bounded
    // pipe can deadlock if a malformed worker fills the pipe first.
    let mut reader = Some(thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout
            .take(MAX_WORKER_RESPONSE_BYTES + 1)
            .read_to_end(&mut bytes);
        read.map(|_| bytes)
    }));
    let wrote = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("worker stdin unavailable"))
        .and_then(|mut stdin| sakura_ai_proto::write_request(&mut stdin, request));
    if wrote.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        let _ = join_worker_output(&mut reader);
        return terminal(AiTextStatus::WorkerError, "worker_write");
    }
    let started = Instant::now();
    loop {
        if cancel.load(Ordering::Acquire) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = join_worker_output(&mut reader);
            return terminal(AiTextStatus::Cancelled, "cancelled");
        }
        if started.elapsed() >= WORKER_DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            let _ = join_worker_output(&mut reader);
            return terminal(AiTextStatus::Timeout, "worker_timeout");
        }
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_worker_output(&mut reader);
                return terminal(AiTextStatus::WorkerError, "worker_exit");
            }
            Ok(None) => thread::sleep(WAIT_QUANTUM),
        }
    }
    let Ok(bytes) = join_worker_output(&mut reader) else {
        return terminal(AiTextStatus::WorkerError, "worker_output_read");
    };
    if bytes.len() as u64 > MAX_WORKER_RESPONSE_BYTES {
        return terminal(AiTextStatus::WorkerError, "worker_output_size");
    }
    let response = match sakura_ai_proto::read_response(&bytes[..]) {
        Ok(response) if response.id == request.id => response,
        _ => return terminal(AiTextStatus::WorkerError, "worker_protocol"),
    };
    AiTextResult {
        status: map_status(response.status),
        result: response.result,
        model: response.model,
        provider: String::new(),
        style: String::new(),
        error_code: response.error_code,
        latency_ms: response.latency_ms,
        input_tokens: response.input_tokens,
        output_tokens: response.output_tokens,
        cached_tokens: response.cached_tokens,
        attempts: response.attempts,
    }
}

fn join_worker_output(
    reader: &mut Option<thread::JoinHandle<io::Result<Vec<u8>>>>,
) -> io::Result<Vec<u8>> {
    reader
        .take()
        .ok_or_else(|| io::Error::other("worker output already joined"))?
        .join()
        .map_err(|_| io::Error::other("worker output reader panicked"))?
}

fn map_status(status: Status) -> AiTextStatus {
    match status {
        Status::Applied => AiTextStatus::Applied,
        Status::MissingKey => AiTextStatus::MissingKey,
        Status::Timeout => AiTextStatus::Timeout,
        Status::ApiError | Status::HttpError => AiTextStatus::ApiError,
        Status::TooLarge | Status::MalformedResponse | Status::WorkerError => {
            AiTextStatus::WorkerError
        }
    }
}

fn terminal(status: AiTextStatus, code: &str) -> AiTextResult {
    AiTextResult {
        status,
        result: String::new(),
        model: sakura_ai_proto::MODEL.to_owned(),
        provider: String::new(),
        style: String::new(),
        error_code: code.to_owned(),
        latency_ms: 0,
        input_tokens: 0,
        output_tokens: 0,
        cached_tokens: 0,
        attempts: 0,
    }
}

fn map_provider(value: AiProvider) -> Provider {
    match value {
        AiProvider::OpenAi => Provider::OpenAi,
        AiProvider::AzureOpenAi => Provider::AzureOpenAi,
        AiProvider::AwsBedrock => Provider::AwsBedrock,
        AiProvider::Cloudflare => Provider::Cloudflare,
        AiProvider::Custom => Provider::Custom,
        AiProvider::ChatGptCodex => Provider::ChatGptCodex,
    }
}

fn map_auth(value: AiAuth) -> Auth {
    match value {
        AiAuth::Bearer => Auth::Bearer,
        AiAuth::ApiKey => Auth::ApiKey,
        AiAuth::None => Auth::None,
    }
}

fn map_style(value: AiStyle) -> Style {
    match value {
        AiStyle::Spoken => Style::Spoken,
        AiStyle::Polite => Style::Polite,
        AiStyle::Business => Style::Business,
        AiStyle::Government => Style::Government,
        AiStyle::Technical => Style::Technical,
        AiStyle::Academic => Style::Academic,
        AiStyle::Contract => Style::Contract,
        AiStyle::Novel => Style::Novel,
        AiStyle::Social => Style::Social,
        AiStyle::English => Style::English,
    }
}

fn map_effort(value: AiEffort) -> Effort {
    match value {
        AiEffort::ProviderDefault => Effort::ProviderDefault,
        AiEffort::None => Effort::None,
        AiEffort::Low => Effort::Low,
        AiEffort::Medium => Effort::Medium,
        AiEffort::High => Effort::High,
        AiEffort::XHigh => Effort::XHigh,
        AiEffort::Max => Effort::Max,
    }
}

fn map_service_tier(value: AiServiceTier) -> ServiceTier {
    match value {
        AiServiceTier::ProviderDefault => ServiceTier::ProviderDefault,
        AiServiceTier::Priority => ServiceTier::Priority,
    }
}

fn provider_name(value: AiProvider) -> &'static str {
    match value {
        AiProvider::OpenAi => "openai",
        AiProvider::AzureOpenAi => "azure-openai",
        AiProvider::AwsBedrock => "aws-bedrock",
        AiProvider::Cloudflare => "cloudflare",
        AiProvider::Custom => "custom",
        AiProvider::ChatGptCodex => "chatgpt-codex-cli",
    }
}

fn style_name(value: AiStyle) -> &'static str {
    match value {
        AiStyle::Spoken => "spoken",
        AiStyle::Polite => "polite",
        AiStyle::Business => "business",
        AiStyle::Government => "government",
        AiStyle::Technical => "technical",
        AiStyle::Academic => "academic",
        AiStyle::Contract => "contract",
        AiStyle::Novel => "novel",
        AiStyle::Social => "social",
        AiStyle::English => "english",
    }
}

fn default_worker_path() -> PathBuf {
    let mut path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("sakura_engine.exe"));
    path.set_file_name("sakura_ai_worker.exe");
    path
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn text_hash(bytes: &[u8]) -> u64 {
    // Stable FNV-1a is sufficient for duplicate suppression. A collision only
    // rejects a request for two seconds; source text itself is never retained.
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: u64, owner: u64, session: SessionId, complete: bool) -> Job {
        Job {
            id,
            owner,
            session,
            cancel: Arc::new(AtomicBool::new(false)),
            signature: RequestSignature {
                operation: AiTextOperation::Transform,
                text_hash: text_hash(format!("job-{id}").as_bytes()),
                text_bytes: 5,
            },
            detached: false,
            state: if complete {
                JobState::Complete(terminal(AiTextStatus::Applied, ""))
            } else {
                JobState::Pending
            },
        }
    }

    #[test]
    fn capacity_duplicate_missing_and_cancel_are_explicit_terminals() {
        let service = AiTextService::new(PathBuf::from("definitely-missing-worker.exe"));
        let owner = service.allocate_owner();
        let first = service
            .start(owner, 1, AiTextOperation::Transform, "text")
            .expect("start");
        assert_eq!(
            service.start(owner, 1, AiTextOperation::Transform, "again"),
            Err(StartError::Duplicate)
        );
        assert!(service.cancel(owner, 1, first));
        assert_eq!(service.poll(owner, 1, first), Poll::Missing);
        assert!(!service.cancel(owner, 1, first));
        assert_eq!(service.poll(owner, 99, first), Poll::Missing);
    }

    #[test]
    fn empty_and_over_limit_requests_never_spawn() {
        let service = AiTextService::new(PathBuf::from("unused.exe"));
        let owner = service.allocate_owner();
        assert_eq!(
            service.start(owner, 1, AiTextOperation::Transform, ""),
            Err(StartError::Invalid)
        );
        assert_eq!(
            service.start(
                owner,
                1,
                AiTextOperation::Transform,
                &"x".repeat(sakura_ai_proto::MAX_TEXT_BYTES + 1)
            ),
            Err(StartError::Invalid)
        );
    }

    #[test]
    fn worker_status_mapping_is_total() {
        let cases = [
            (Status::Applied, AiTextStatus::Applied),
            (Status::MissingKey, AiTextStatus::MissingKey),
            (Status::TooLarge, AiTextStatus::WorkerError),
            (Status::HttpError, AiTextStatus::ApiError),
            (Status::ApiError, AiTextStatus::ApiError),
            (Status::MalformedResponse, AiTextStatus::WorkerError),
            (Status::Timeout, AiTextStatus::Timeout),
            (Status::WorkerError, AiTextStatus::WorkerError),
        ];
        for (worker, engine) in cases {
            assert_eq!(map_status(worker), engine);
        }
    }

    #[test]
    fn poll_cancel_owner_and_session_have_exact_identity_and_terminal_ownership() {
        let service = AiTextService::new(PathBuf::from("unused.exe"));
        {
            let mut state = lock(&service.state);
            state.jobs.extend([
                job(1, 10, 100, false),
                job(2, 10, 101, true),
                job(3, 11, 100, true),
            ]);
        }
        assert_eq!(service.poll(99, 100, 1), Poll::Missing);
        assert_eq!(service.poll(10, 999, 1), Poll::Missing);
        assert_eq!(service.poll(10, 100, 1), Poll::Pending);
        assert!(matches!(service.poll(11, 100, 3), Poll::Complete(_)));
        assert_eq!(service.poll(11, 100, 3), Poll::Missing);

        service.cancel_session(10, 101);
        {
            let state = lock(&service.state);
            assert!(state.jobs.iter().all(|value| value.id != 2));
            assert!(state
                .jobs
                .iter()
                .any(|value| value.id == 1 && !value.detached));
        }
        service.cancel_owner(10);
        {
            let state = lock(&service.state);
            let retained = state
                .jobs
                .iter()
                .find(|value| value.id == 1)
                .expect("pending");
            assert!(retained.detached);
            assert!(retained.cancel.load(Ordering::Acquire));
        }
        assert_eq!(service.poll(10, 100, 1), Poll::Missing);
        assert!(!service.cancel(10, 100, 1));
    }

    #[test]
    fn capacity_and_cooldown_are_global_while_owner_duplicate_is_scoped() {
        let service = AiTextService::new(PathBuf::from("unused.exe"));
        {
            let mut state = lock(&service.state);
            state.jobs.push(job(1, 10, 100, false));
        }
        assert_eq!(
            service.start(11, 100, AiTextOperation::Transform, "different"),
            Err(StartError::Capacity)
        );
        {
            let mut state = lock(&service.state);
            state.jobs.clear();
            state.cooldowns.push(Cooldown {
                signature: RequestSignature {
                    operation: AiTextOperation::Proofread,
                    text_hash: text_hash(b"same"),
                    text_bytes: 4,
                },
                until: Instant::now() + Duration::from_secs(10),
            });
        }
        assert_eq!(
            service.start(11, 100, AiTextOperation::Proofread, "same"),
            Err(StartError::Duplicate)
        );
    }

    #[test]
    fn mappings_and_hashes_match_independent_examples() {
        assert_eq!(map_provider(AiProvider::OpenAi), Provider::OpenAi);
        assert_eq!(map_provider(AiProvider::AzureOpenAi), Provider::AzureOpenAi);
        assert_eq!(map_provider(AiProvider::AwsBedrock), Provider::AwsBedrock);
        assert_eq!(map_provider(AiProvider::Cloudflare), Provider::Cloudflare);
        assert_eq!(map_provider(AiProvider::Custom), Provider::Custom);
        assert_eq!(
            map_provider(AiProvider::ChatGptCodex),
            Provider::ChatGptCodex
        );
        for value in AiAuth::ALL {
            assert_eq!(map_auth(value) as u8, value as u8 + 1);
        }
        for value in AiStyle::ALL {
            assert!(!style_name(value).is_empty());
            assert_eq!(map_style(value) as u8, value as u8 + 1);
        }
        for value in AiEffort::ALL {
            assert_eq!(map_effort(value) as u8, value as u8 + 1);
        }
        for value in AiServiceTier::ALL {
            assert_eq!(map_service_tier(value) as u8, value as u8 + 1);
        }
        for provider in AiProvider::ALL {
            assert!(!provider_name(provider).is_empty());
        }
        assert_eq!(text_hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(text_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(text_hash(b"ab"), text_hash(b"ba"));
    }
}
