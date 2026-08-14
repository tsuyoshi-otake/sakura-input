use std::io::{self, Write};
use std::time::Instant;

use sakura_ai_proto::{Provider, Request, Response, Status, MODEL};
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::Threading::GetCurrentProcess;

mod codex_cli;
mod openai;

struct JobHandle(HANDLE);

impl Drop for JobHandle {
    fn drop(&mut self) {
        // SAFETY: this wrapper exclusively owns the job handle.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// Put this worker in a kill-on-close job before it can launch Codex. Children
/// inherit the job, so engine cancellation or timeout cannot orphan node.exe or
/// codex.exe. The handle intentionally lives until process teardown.
fn install_process_job() -> io::Result<()> {
    // SAFETY: no security attributes or public job name are supplied.
    let job = JobHandle(
        unsafe { CreateJobObjectW(None, PCWSTR::null()) }
            .map_err(|error| io::Error::other(error.to_string()))?,
    );
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: `limits` is the exact structure selected by the information class.
    unsafe {
        SetInformationJobObject(
            job.0,
            JobObjectExtendedLimitInformation,
            (&raw const limits).cast(),
            u32::try_from(std::mem::size_of_val(&limits))
                .map_err(|_| io::Error::other("job information size"))?,
        )
        .map_err(|error| io::Error::other(error.to_string()))?;
        AssignProcessToJobObject(job.0, GetCurrentProcess())
            .map_err(|error| io::Error::other(error.to_string()))?;
    }
    std::mem::forget(job);
    Ok(())
}

fn terminal(request: &Request, started: Instant, execution: openai::Execution) -> Response {
    let attempts = execution.attempts;
    match execution.outcome {
        openai::Outcome::Success(success) => Response {
            id: request.id,
            status: Status::Applied,
            result: success.text,
            model: success.model,
            error_code: String::new(),
            latency_ms: elapsed_ms(started),
            input_tokens: success.input_tokens,
            output_tokens: success.output_tokens,
            cached_tokens: success.cached_tokens,
            attempts,
        },
        openai::Outcome::Failure { status, code } => Response {
            id: request.id,
            status,
            result: String::new(),
            model: MODEL.to_owned(),
            error_code: code.to_owned(),
            latency_ms: elapsed_ms(started),
            input_tokens: 0,
            output_tokens: 0,
            cached_tokens: 0,
            attempts,
        },
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn run() -> io::Result<()> {
    install_process_job()?;
    let mut request = sakura_ai_proto::read_request(io::stdin().lock())?;
    let started = Instant::now();
    let outcome = if request.provider != Provider::ChatGptCodex
        && request.auth != sakura_ai_proto::Auth::None
        && request.api_key.trim().is_empty()
    {
        openai::Execution::without_attempt(openai::Outcome::Failure {
            status: Status::MissingKey,
            code: "missing_api_key",
        })
    } else if request.provider == Provider::ChatGptCodex {
        codex_cli::execute(&request)
    } else {
        openai::execute(&request)
    };
    // SAFETY: zero is valid UTF-8. The key is not observed again before drop.
    unsafe { request.api_key.as_bytes_mut() }.fill(0);
    request.api_key.clear();
    let response = terminal(&request, started, outcome);
    let mut stdout = io::stdout().lock();
    sakura_ai_proto::write_response(&mut stdout, &response)?;
    stdout.flush()
}

fn main() {
    if let Err(error) = run() {
        // Content-free by contract: never print the request, response, or key.
        eprintln!("sakura_ai_worker failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_terminal_contains_no_source_or_result() {
        let request = Request {
            id: 4,
            operation: sakura_ai_proto::Operation::Transform,
            provider: Provider::OpenAi,
            endpoint: "https://api.openai.com/v1".to_owned(),
            auth: sakura_ai_proto::Auth::Bearer,
            api_key: "private-key".to_owned(),
            style: sakura_ai_proto::Style::Polite,
            effort: sakura_ai_proto::Effort::Low,
            service_tier: sakura_ai_proto::ServiceTier::ProviderDefault,
            text: "private".into(),
        };
        let response = terminal(
            &request,
            Instant::now(),
            openai::Execution {
                outcome: openai::Outcome::Failure {
                    status: Status::ApiError,
                    code: "api_error",
                },
                attempts: 1,
            },
        );
        assert!(response.result.is_empty());
        assert_eq!(response.error_code, "api_error");
    }
}
