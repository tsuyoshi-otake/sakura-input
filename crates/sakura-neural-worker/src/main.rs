mod cli;
mod manifest;
mod protocol;
mod sakura_runtime;
mod sakura_scorer;
mod simd;

use std::{env, io, path::Path};

fn fatal(message: impl std::fmt::Display) -> ! {
    eprintln!("sakura_neural_worker fatal: {message}");
    std::process::exit(1)
}

fn initialize(model: &Path) -> (simd::Dispatch, String, sakura_runtime::ModelRuntime) {
    let validated = manifest::validate(model).unwrap_or_else(|error| fatal(error));
    let dispatch = simd::Dispatch::startup().unwrap_or_else(|error| fatal(error));
    let runtime = sakura_runtime::ModelRuntime::load(model).unwrap_or_else(|error| fatal(error));
    (dispatch, validated.model_hash, runtime)
}

fn run_stdio(dispatch: simd::Dispatch, runtime: sakura_runtime::ModelRuntime) {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        match protocol::read(&mut input) {
            Ok(Some(request)) => {
                let deadline = std::time::Instant::now() + sakura_scorer::REQUEST_DEADLINE;
                let result = sakura_scorer::score(&request, &runtime, deadline);
                let write = match result {
                    Ok(scores) => protocol::write_success(
                        &mut output,
                        request.id,
                        dispatch.tier() as u16,
                        &scores,
                    ),
                    Err(_) => {
                        protocol::write_failure(&mut output, request.id, dispatch.tier() as u16)
                    }
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
        cli::Command::SelfTest(force) => {
            assert!(protocol::decode(&[0]).is_err());
            sakura_scorer::self_test().unwrap_or_else(|error| fatal(error));
            sakura_runtime::self_test().unwrap_or_else(|error| fatal(error));
            let dispatch = match force {
                Some(tier) => simd::Dispatch::force_for_self_test(&tier),
                None => simd::Dispatch::startup(),
            }
            .unwrap_or_else(|error| fatal(error));
            simd::self_test(&dispatch).unwrap_or_else(|error| fatal(error));
            println!("self-test passed");
        }
        cli::Command::Probe(model) => {
            let (dispatch, hash, _runtime) = initialize(&model);
            println!(
                "{{\"tier\":\"{}\",\"runtime\":\"onnxruntime\",\"model\":\"{}\",\"model_sha256\":\"{}\",\"status\":\"research_only_gate_a_failed\"}}",
                dispatch.tier().name(),
                manifest::MODEL_ID,
                hash
            );
        }
        cli::Command::Stdio(model) => {
            let (dispatch, _, runtime) = initialize(&model);
            run_stdio(dispatch, runtime);
        }
    }
}
