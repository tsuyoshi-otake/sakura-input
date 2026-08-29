mod cli;
mod manifest;
mod protocol;
mod sakura_runtime;
mod sakura_scorer;

use std::{env, io, path::Path};

fn fatal(message: impl std::fmt::Display) -> ! {
    eprintln!("sakura_neural_worker fatal: {message}");
    std::process::exit(1)
}

fn initialize(model: &Path) -> (String, sakura_runtime::ModelRuntime) {
    let validated = manifest::validate(model).unwrap_or_else(|error| fatal(error));
    let runtime = sakura_runtime::ModelRuntime::load(model).unwrap_or_else(|error| fatal(error));
    (validated.model_hash, runtime)
}

fn probe_json(model_hash: &str) -> String {
    format!(
        "{{\"runtime\":\"onnxruntime\",\"model\":\"{}\",\"model_sha256\":\"{}\",\"status\":\"{}\"}}",
        manifest::MODEL_ID,
        model_hash,
        manifest::RUNTIME_STATUS,
    )
}

fn run_stdio(runtime: sakura_runtime::ModelRuntime) {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        match protocol::read(&mut input) {
            Ok(Some(request)) => {
                let deadline = std::time::Instant::now() + sakura_scorer::REQUEST_DEADLINE;
                let result = sakura_scorer::score(&request, &runtime, deadline);
                let write = match result {
                    Ok(scores) => protocol::write_success(&mut output, request.id, &scores),
                    Err(_) => protocol::write_failure(&mut output, request.id),
                };
                if write.is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(error) => fatal(error),
        }
    }
}

fn main() {
    let args: Vec<String> = env::args().collect();
    match cli::parse(&args).unwrap_or_else(|error| fatal(error)) {
        cli::Command::SelfTest => {
            assert!(protocol::decode(&[0]).is_err());
            sakura_scorer::self_test().unwrap_or_else(|error| fatal(error));
            sakura_runtime::self_test().unwrap_or_else(|error| fatal(error));
            println!("self-test passed");
        }
        cli::Command::Probe(model) => {
            let (hash, _runtime) = initialize(&model);
            println!("{}", probe_json(&hash));
        }
        cli::Command::Stdio(model) => {
            let (_, runtime) = initialize(&model);
            run_stdio(runtime);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn probe_reports_only_runtime_owned_execution_metadata() {
        let value: serde_json::Value = serde_json::from_str(&super::probe_json("abc")).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "runtime": "onnxruntime",
                "model": super::manifest::MODEL_ID,
                "model_sha256": "abc",
                "status": super::manifest::RUNTIME_STATUS,
            })
        );
    }
}
