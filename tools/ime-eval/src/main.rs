use std::process::ExitCode;

fn main() -> ExitCode {
    match sakura_ime_eval::cli::run(std::env::args_os().skip(1)) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("ime-eval: {error}");
            ExitCode::from(2)
        }
    }
}
