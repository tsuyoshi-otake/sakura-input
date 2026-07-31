//! The engine process.
//!
//! Windowless: it is started by a per-user logon task and lives until the
//! session ends (DESIGN 4.3), so a console subsystem binary would flash a
//! window at every logon. `--verbose` attaches to the console of whatever
//! started it, which is how it stays diagnosable when run by hand without
//! costing every user a black rectangle at logon.

#![cfg(windows)]
#![windows_subsystem = "windows"]

use sakura_engine::server::Server;

/// Clean exit: a client asked the engine to stop.
const EXIT_OK: i32 = 0;
/// The engine could not start.
const EXIT_FAILED: i32 = 1;
/// Another engine already owns this session's pipe. Not an error — the
/// logon task and a manual launch can race, and the loser should be quiet
/// about it rather than leave a failure in the task scheduler's history.
const EXIT_ALREADY_RUNNING: i32 = 2;
/// This machine is below the instruction-set baseline the binary was built
/// for (DESIGN 3.2).
const EXIT_UNSUPPORTED_CPU: i32 = 3;

fn main() {
    let verbose = std::env::args().skip(1).any(|arg| arg == "--verbose");
    if verbose {
        attach_parent_console();
    }

    // Before the pipe, before the dictionary, before anything can be waiting
    // on the answer: which vector kernels this machine gets, decided once
    // (DESIGN 3.2). Naming the tier in the log is what makes a later
    // "why is it slower here?" a question with an answer.
    match sakura_core::cpu::startup() {
        Ok(tier) => {
            if verbose {
                eprintln!("sakura-engine: vector kernels: {}", tier.name());
            }
        }
        Err(error) => {
            // The installer refuses to install on such a machine, so
            // reaching this means the files were copied here by hand.
            if verbose {
                eprintln!("sakura-engine: {error}");
            }
            std::process::exit(EXIT_UNSUPPORTED_CPU);
        }
    }

    std::process::exit(match run(verbose) {
        Ok(()) => EXIT_OK,
        Err(error) if is_name_taken(&error) => {
            if verbose {
                eprintln!("sakura-engine: another engine already has this session's pipe");
            }
            EXIT_ALREADY_RUNNING
        }
        Err(error) => {
            if verbose {
                eprintln!("sakura-engine: {error}");
            }
            EXIT_FAILED
        }
    });
}

fn run(verbose: bool) -> windows::core::Result<()> {
    let server = Server::new(verbose)?;
    if verbose {
        eprintln!("sakura-engine: listening on {}", server.pipe_name());
    }
    server.run()
}

/// What `FILE_FLAG_FIRST_PIPE_INSTANCE` reports when the name is already
/// taken. It denies access rather than saying "in use", because from the
/// object manager's point of view we asked to own a name somebody else
/// already owns.
fn is_name_taken(error: &windows::core::Error) -> bool {
    use windows::core::HRESULT;
    use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_PIPE_BUSY};
    let code = error.code();
    code == HRESULT::from_win32(ERROR_ACCESS_DENIED.0)
        || code == HRESULT::from_win32(ERROR_PIPE_BUSY.0)
}

/// Borrows the launching terminal's console so `--verbose` output has
/// somewhere to go. Failure is expected and ignored: started from a logon
/// task there is no parent console, which is exactly the case this binary
/// is built windowless for.
fn attach_parent_console() {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: no arguments to get wrong, and the only failure mode —
    // having no parent console, or already having one — is reported as an
    // error rather than as undefined behaviour.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
    }
}
