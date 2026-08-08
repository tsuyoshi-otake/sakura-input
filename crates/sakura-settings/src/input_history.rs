//! Developer input-history viewing, export, and administration.

use std::io;
use std::path::Path;
use std::time::Duration;

use sakura_engine::input_history::{
    clear_path, read_snapshot, InputHistorySnapshot, INPUT_HISTORY_FORMAT_VERSION,
};
use sakura_ipc::diagnostics::{record_timeout, TimeoutOperation};
use sakura_ipc::{Client, Fault};
use sakura_proto::{Request, Response, PROTOCOL_VERSION};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};

use crate::storage::atomic_write;

const ADMIN_CALL_BUDGET: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearRoute {
    LiveEngine,
    Offline { cleared_records: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushRoute {
    LiveEngine,
    Offline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryStats {
    pub active: bool,
    pub dropped_events: u64,
    pub persistence_failures: u64,
    pub excluded_unclassified_events: u64,
    pub excluded_sensitive_events: u64,
    pub excluded_test_only_events: u64,
    pub live: bool,
}

pub fn view(path: &Path) -> io::Result<InputHistorySnapshot> {
    match read_snapshot(path) {
        Ok(snapshot) => Ok(snapshot),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(InputHistorySnapshot {
            format_version: INPUT_HISTORY_FORMAT_VERSION,
            records: Vec::new(),
            ignored_tail_bytes: 0,
        }),
        Err(error) => Err(error),
    }
}

pub fn export(source: &Path, destination: &Path) -> io::Result<usize> {
    // A missing file means the developer-history service has never been
    // started for this user, so there cannot be queued records to flush. This
    // also keeps an empty export independent of whichever engine binary may
    // happen to be running while settings tests execute.
    if source.exists() {
        let _ = flush(source)?;
    }
    let snapshot = view(source)?;
    atomic_write(destination, snapshot.to_tsv().as_bytes())?;
    Ok(snapshot.records.len())
}

pub fn flush(_path: &Path) -> io::Result<FlushRoute> {
    let mut client = match Client::connect(ADMIN_CALL_BUDGET) {
        Ok(client) => client,
        Err(error) if engine_is_definitely_absent(&error) => return Ok(FlushRoute::Offline),
        Err(error) => return Err(fault("connect to engine", error)),
    };
    handshake(&mut client)?;
    match client.call(&Request::FlushInputHistory, ADMIN_CALL_BUDGET) {
        Ok(Response::Ok) => Ok(FlushRoute::LiveEngine),
        Ok(Response::Error(code)) => Err(io::Error::other(format!(
            "engine could not flush input history: {code:?}"
        ))),
        Ok(response) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected input-history flush response: {response:?}"),
        )),
        Err(Fault::Timeout) => {
            let _ = record_timeout(TimeoutOperation::Administration);
            Err(fault("flush input history through engine", Fault::Timeout))
        }
        Err(error) => Err(fault("flush input history through engine", error)),
    }
}

pub fn stats(_path: &Path) -> io::Result<HistoryStats> {
    let mut client = match Client::connect(ADMIN_CALL_BUDGET) {
        Ok(client) => client,
        Err(error) if engine_is_definitely_absent(&error) => {
            return Ok(HistoryStats {
                active: false,
                dropped_events: 0,
                persistence_failures: 0,
                excluded_unclassified_events: 0,
                excluded_sensitive_events: 0,
                excluded_test_only_events: 0,
                live: false,
            })
        }
        Err(error) => return Err(fault("connect to engine", error)),
    };
    handshake(&mut client)?;
    match client.call(&Request::InputHistoryStats, ADMIN_CALL_BUDGET) {
        Ok(Response::InputHistoryStats {
            active,
            dropped_events,
            persistence_failures,
            excluded_unclassified_events,
            excluded_sensitive_events,
            excluded_test_only_events,
        }) => Ok(HistoryStats {
            active,
            dropped_events,
            persistence_failures,
            excluded_unclassified_events,
            excluded_sensitive_events,
            excluded_test_only_events,
            live: true,
        }),
        Ok(Response::Error(code)) => Err(io::Error::other(format!(
            "engine could not read input-history stats: {code:?}"
        ))),
        Ok(response) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected input-history stats response: {response:?}"),
        )),
        Err(Fault::Timeout) => {
            let _ = record_timeout(TimeoutOperation::Administration);
            Err(fault(
                "read input-history stats through engine",
                Fault::Timeout,
            ))
        }
        Err(error) => Err(fault("read input-history stats through engine", error)),
    }
}

pub fn clear(path: &Path) -> io::Result<ClearRoute> {
    let mut client = match Client::connect(ADMIN_CALL_BUDGET) {
        Ok(client) => client,
        Err(error) if engine_is_definitely_absent(&error) => return clear_offline(path),
        Err(error) => return Err(fault("connect to engine", error)),
    };
    handshake(&mut client)?;
    match client.call(&Request::ClearInputHistory, ADMIN_CALL_BUDGET) {
        Ok(Response::Ok) => Ok(ClearRoute::LiveEngine),
        Ok(Response::Error(code)) => Err(io::Error::other(format!(
            "engine could not clear input history: {code:?}"
        ))),
        Ok(response) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected input-history clear response: {response:?}"),
        )),
        Err(Fault::Timeout) => {
            let _ = record_timeout(TimeoutOperation::Administration);
            Err(fault("clear input history through engine", Fault::Timeout))
        }
        Err(error) => Err(fault("clear input history through engine", error)),
    }
}

pub fn clear_offline(path: &Path) -> io::Result<ClearRoute> {
    Ok(ClearRoute::Offline {
        cleared_records: clear_path(path)?,
    })
}

fn handshake(client: &mut Client) -> io::Result<()> {
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
            let _ = record_timeout(TimeoutOperation::Administration);
            Err(fault("negotiate with engine", Fault::Timeout))
        }
        Err(error) => Err(fault("negotiate with engine", error)),
    }
}

fn fault(action: &str, error: Fault) -> io::Error {
    let kind = match error {
        Fault::Timeout => io::ErrorKind::TimedOut,
        Fault::Disconnected => io::ErrorKind::BrokenPipe,
        Fault::Protocol(_) | Fault::Desynchronized => io::ErrorKind::InvalidData,
        Fault::Encode(_) => io::ErrorKind::InvalidInput,
        Fault::Os(_) => io::ErrorKind::Other,
    };
    io::Error::new(kind, format!("{action}: {error}"))
}

fn engine_is_definitely_absent(error: &Fault) -> bool {
    let Fault::Os(error) = error else {
        return false;
    };
    let raw = error.code().0 as u32;
    let file_not_found = 0x8007_0000 | ERROR_FILE_NOT_FOUND.0;
    let path_not_found = 0x8007_0000 | ERROR_PATH_NOT_FOUND.0;
    raw == file_not_found || raw == path_not_found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(1);

    fn temporary_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "sakura-settings-input-history-{}-{name}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn missing_history_views_and_exports_as_empty() {
        let directory = temporary_path("missing");
        let source = directory.join("missing.bin");
        let destination = directory.join("history.tsv");
        assert!(view(&source).expect("view").records.is_empty());
        assert_eq!(export(&source, &destination).expect("export"), 0);
        assert!(std::fs::read_to_string(destination)
            .expect("TSV")
            .contains("kind\tsequence"));
        let _ = std::fs::remove_dir_all(directory);
    }
}
