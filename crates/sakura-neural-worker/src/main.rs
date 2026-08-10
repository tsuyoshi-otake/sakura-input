mod cli;
mod manifest;
mod protocol;
mod runtime;
mod scorer;
mod simd;
mod tokenizer;

use std::{env, fs, io, path::Path};

fn fatal(message: impl std::fmt::Display) -> ! {
    eprintln!("sakura_neural_worker fatal: {message}");
    std::process::exit(1)
}

fn initialize(
    model: &Path,
) -> (
    simd::Dispatch,
    String,
    tokenizer::Tokenizer,
    runtime::ModelRuntime,
) {
    let hash = manifest::validate(model).unwrap_or_else(|error| fatal(error));
    let vocabulary = fs::read_to_string(model.join("vocab.txt"))
        .unwrap_or_else(|_| fatal("unable to read model vocabulary"));
    let tokenizer =
        tokenizer::Tokenizer::from_vocab(&vocabulary).unwrap_or_else(|error| fatal(error));
    let dispatch = simd::Dispatch::startup().unwrap_or_else(|error| fatal(error));
    let runtime = runtime::ModelRuntime::load(model).unwrap_or_else(|error| fatal(error));
    (dispatch, hash, tokenizer, runtime)
}

fn run_stdio(
    dispatch: simd::Dispatch,
    tokenizer: tokenizer::Tokenizer,
    runtime: runtime::ModelRuntime,
) {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    loop {
        match protocol::read(&mut input) {
            Ok(Some(request)) => {
                let deadline = std::time::Instant::now() + scorer::REQUEST_DEADLINE;
                let result = scorer::score(&request, &tokenizer, &dispatch, &runtime, deadline);
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
            tokenizer::self_test().unwrap_or_else(|error| fatal(error));
            scorer::self_test().unwrap_or_else(|error| fatal(error));
            runtime::self_test().unwrap_or_else(|error| fatal(error));
            let dispatch = match force {
                Some(tier) => simd::Dispatch::force_for_self_test(&tier),
                None => simd::Dispatch::startup(),
            }
            .unwrap_or_else(|error| fatal(error));
            simd::self_test(&dispatch).unwrap_or_else(|error| fatal(error));
            println!("self-test passed");
        }
        cli::Command::Probe(model) => {
            let (dispatch, hash, _tokenizer, _runtime) = initialize(&model);
            println!(
                "{{\"tier\":\"{}\",\"runtime\":\"onnxruntime\",\"model_sha256\":\"{}\"}}",
                dispatch.tier().name(),
                hash
            );
        }
        cli::Command::Stdio(model) => {
            let (dispatch, _, tokenizer, runtime) = initialize(&model);
            run_stdio(dispatch, tokenizer, runtime);
        }
    }
}
