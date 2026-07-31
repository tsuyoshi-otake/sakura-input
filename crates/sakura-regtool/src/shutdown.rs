//! `--stop`: ask the engine to exit, then confirm that it did.
//!
//! The installer runs this before replacing files (DESIGN 12.3), so
//! "returned 0" has to mean *the engine is gone*, not *the engine was
//! asked*. A file swap that races a still-running engine is a locked file
//! and a failed upgrade, and the race is easy to lose — the engine answers
//! the shutdown request before it exits, precisely so the asker gets an
//! acknowledgement, which means the reply arrives while the process is
//! still alive.
//!
//! So the acknowledgement is not the end of the check. Afterwards this
//! waits for the pipe *name* to disappear, which happens when the last
//! handle to it closes, which happens when the process ends. That is an
//! observation of the thing we actually care about rather than a proxy for
//! it.

use std::time::{Duration, Instant};

use sakura_ipc::{pipe_name, Client, Fault};
use sakura_proto::{Request, Response};
use windows::core::HRESULT;
use windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;

/// How often to look again while waiting for the engine to disappear.
const POLL: Duration = Duration::from_millis(25);

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing was listening. Not an error: this runs on uninstall paths
    /// where the engine may already have been stopped, or never started.
    NotRunning,
    /// Asked, acknowledged, and confirmed gone.
    Stopped,
}

pub fn stop(budget: Duration) -> Result<Outcome, String> {
    let deadline = Instant::now() + budget;
    let name = pipe_name().map_err(|error| format!("cannot name the engine's pipe: {error}"))?;

    let mut client = match Client::connect_to(&name) {
        Ok(client) => client,
        Err(error) if is_absent(&error) => return Ok(Outcome::NotRunning),
        Err(error) => return Err(format!("cannot reach the engine: {error}")),
    };

    match client.call(&Request::Shutdown, remaining(deadline)) {
        Ok(Response::Error(code)) => return Err(format!("the engine refused to stop: {code:?}")),
        // Any other answer is an acknowledgement; the engine has no reason
        // to reply to a shutdown with a composition.
        Ok(_) => {}
        // It closed the pipe instead of answering. Unusual ordering, but
        // the outcome asked for is the outcome that happened.
        Err(Fault::Disconnected) => {}
        Err(error) => return Err(format!("the engine did not accept the request: {error}")),
    }

    // Release our own handle first: while it is open, the pipe's name
    // stays resolvable and the wait below could never finish.
    drop(client);

    while Instant::now() < deadline {
        match Client::connect_to(&name) {
            Err(error) if is_absent(&error) => return Ok(Outcome::Stopped),
            // Still there — either the process is winding down, or another
            // instance is still accepting. Either way, not gone yet.
            Ok(client) => drop(client),
            Err(_) => {}
        }
        std::thread::sleep(POLL);
    }

    Err(format!(
        "the engine acknowledged the request but was still running {} ms later",
        budget.as_millis()
    ))
}

fn remaining(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

/// Distinguishes "no engine" from "an engine that will not talk to us".
/// Only the first is success, so this checks the specific code rather than
/// treating every connect failure as absence — a pipe we are *denied*
/// looks nothing like a pipe that is not there, and quietly reporting
/// "stopped" for it would let an upgrade proceed into a locked file.
fn is_absent(fault: &Fault) -> bool {
    match fault {
        Fault::Os(error) => error.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no engine running — the state of any machine that has not
    /// started one — `--stop` must succeed rather than report a failure
    /// the uninstaller would halt on.
    #[test]
    fn stopping_an_engine_that_is_not_running_is_not_a_failure() {
        // Guard: if something on this machine is serving the pipe, the
        // assertion below would be testing the opposite case.
        let name = pipe_name().expect("a pipe name");
        if Client::connect_to(&name).is_ok() {
            return;
        }
        assert_eq!(stop(Duration::from_millis(500)), Ok(Outcome::NotRunning));
    }
}
