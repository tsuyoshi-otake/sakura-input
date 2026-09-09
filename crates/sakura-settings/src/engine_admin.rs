//! Shared plumbing for the settings tool's administrative calls to a running
//! engine.
//!
//! Connecting, negotiating and classifying a fault were written out
//! identically in every module that talks to the control pipe. They are one
//! decision, not several: which endpoint an administrative call may use, which
//! server it is willing to trust, how long it waits, and what an expired
//! deadline is recorded as. Keeping one copy is what makes those answers the
//! same everywhere rather than the same by coincidence.

use std::io;
use std::time::Duration;

use sakura_ipc::diagnostics::{record_timeout, TimeoutOperation};
use sakura_ipc::{Client, Endpoint, Fault, ServerTrustPolicy};
use sakura_proto::{Request, Response, PROTOCOL_VERSION};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};

/// How long any one administrative call may wait.
///
/// Far longer than a keystroke's budget on purpose: nothing here is on the
/// input path, and a settings command that gave up early would report a
/// healthy engine as missing.
pub const ADMIN_CALL_BUDGET: Duration = Duration::from_secs(2);

pub fn handshake(client: &mut Client) -> io::Result<()> {
    match client.call(
        &Request::Hello {
            client_version: PROTOCOL_VERSION,
        },
        ADMIN_CALL_BUDGET,
    ) {
        Ok(Response::Hello { server_version, .. }) if server_version == PROTOCOL_VERSION => Ok(()),
        Ok(Response::Error(code)) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("engine rejected settings handshake: {code:?}"),
        )),
        Ok(response) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected settings handshake response: {response:?}"),
        )),
        Err(Fault::Timeout) => {
            let _ = record_timeout(TimeoutOperation::Administration, client.last_call_elapsed());
            Err(fault("negotiate with engine", Fault::Timeout))
        }
        Err(error) => Err(fault("negotiate with engine", error)),
    }
}

pub fn fault(action: &str, error: Fault) -> io::Error {
    let kind = match error {
        Fault::Timeout | Fault::DeadlineExpired => io::ErrorKind::TimedOut,
        Fault::Disconnected => io::ErrorKind::BrokenPipe,
        Fault::Protocol(_) | Fault::Desynchronized => io::ErrorKind::InvalidData,
        Fault::Encode(_) => io::ErrorKind::InvalidInput,
        Fault::UntrustedServer { .. } => io::ErrorKind::PermissionDenied,
        Fault::Os(_) => io::ErrorKind::Other,
    };
    io::Error::new(kind, format!("{action}: {error}"))
}

pub fn installed_root_policy() -> io::Result<ServerTrustPolicy> {
    let executable = std::env::current_exe()?;
    let root = executable
        .parent()
        .and_then(|release| release.parent())
        .and_then(|versions| versions.parent())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "settings executable is not versioned",
            )
        })?;
    Ok(ServerTrustPolicy::InstalledRoot(root.to_path_buf()))
}

/// True only when Windows has said the pipe does not exist.
///
/// Every other failure — a busy pipe, a refused connection, an untrusted
/// server — leaves open the possibility that an engine is running and holding
/// the store, so a caller must not fall back to touching files directly on the
/// strength of it.
pub fn engine_is_definitely_absent(error: &Fault) -> bool {
    let Fault::Os(error) = error else {
        return false;
    };
    let raw = error.code().0 as u32;
    let file_not_found = 0x8007_0000 | ERROR_FILE_NOT_FOUND.0;
    let path_not_found = 0x8007_0000 | ERROR_PATH_NOT_FOUND.0;
    raw == file_not_found || raw == path_not_found
}

/// Opens a verified control-pipe connection and completes the handshake.
pub fn connect() -> Result<Client, ConnectError> {
    let policy = installed_root_policy().map_err(ConnectError::Failed)?;
    let mut client =
        match Client::connect_endpoint_verified(Endpoint::Control, &policy, ADMIN_CALL_BUDGET) {
            Ok(client) => client,
            Err(error) if engine_is_definitely_absent(&error) => return Err(ConnectError::Absent),
            Err(error) => return Err(ConnectError::Failed(fault("connect to engine", error))),
        };
    handshake(&mut client).map_err(ConnectError::Failed)?;
    Ok(client)
}

/// Why an administrative connection did not happen.
///
/// `Absent` is kept apart from `Failed` because the two have opposite
/// consequences: a caller may safely fall back to reading a file when the
/// engine is provably not running, and must not when the reason is unknown.
#[derive(Debug)]
pub enum ConnectError {
    Absent,
    Failed(io::Error),
}
