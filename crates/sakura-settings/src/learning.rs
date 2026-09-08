//! Learning viewer, exporter, and race-safe clear operation.

use std::io;
use std::path::Path;
use std::time::Duration;

use sakura_engine::learning::{
    read_snapshot, LearningService, LearningSnapshot, LEARNING_FORMAT_VERSION,
};
use sakura_ipc::diagnostics::{record_timeout, TimeoutOperation};
use sakura_ipc::{Client, Endpoint, Fault, ServerTrustPolicy};
use sakura_proto::{Request, Response, PROTOCOL_VERSION};
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND};

use crate::storage::atomic_write;

const ADMIN_CALL_BUDGET: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearRoute {
    LiveEngine,
    Offline { cleared_records: u64 },
}

pub fn view(path: &Path) -> io::Result<LearningSnapshot> {
    match read_snapshot(path) {
        Ok(snapshot) => Ok(snapshot),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(LearningSnapshot {
            format_version: LEARNING_FORMAT_VERSION,
            records: Vec::new(),
            ignored_tail_bytes: 0,
        }),
        Err(error) => Err(error),
    }
}

pub fn export(source: &Path, destination: &Path) -> io::Result<usize> {
    let snapshot = view(source)?;
    atomic_write(destination, snapshot.to_tsv().as_bytes())?;
    Ok(snapshot.records.len())
}

/// Clears through the running engine whenever its pipe exists. Direct file
/// access is used only when Windows proves there is no pipe, avoiding a race
/// with a live writer or its maintenance thread.
pub fn clear(path: &Path) -> io::Result<ClearRoute> {
    let policy = installed_root_policy()?;
    let mut client =
        match Client::connect_endpoint_verified(Endpoint::Control, &policy, ADMIN_CALL_BUDGET) {
            Ok(client) => client,
            Err(error) if engine_is_definitely_absent(&error) => return clear_offline(path),
            Err(error) => return Err(fault("connect to engine", error)),
        };
    match client.call(
        &Request::Hello {
            client_version: PROTOCOL_VERSION,
        },
        ADMIN_CALL_BUDGET,
    ) {
        Ok(Response::Hello { server_version, .. }) if server_version == PROTOCOL_VERSION => {}
        Ok(Response::Error(code)) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("engine rejected settings handshake: {code:?}"),
            ))
        }
        Ok(response) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected settings handshake response: {response:?}"),
            ))
        }
        Err(Fault::Timeout) => {
            let _ = record_timeout(TimeoutOperation::Administration);
            return Err(fault("negotiate with engine", Fault::Timeout));
        }
        Err(error) => return Err(fault("negotiate with engine", error)),
    }

    match client.call(&Request::ClearLearning, ADMIN_CALL_BUDGET) {
        Ok(Response::Ok) => Ok(ClearRoute::LiveEngine),
        Ok(Response::Error(code)) => Err(io::Error::other(format!(
            "engine could not clear learning: {code:?}"
        ))),
        Ok(response) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected clear-learning response: {response:?}"),
        )),
        Err(Fault::Timeout) => {
            let _ = record_timeout(TimeoutOperation::Administration);
            Err(fault("clear learning through engine", Fault::Timeout))
        }
        Err(error) => Err(fault("clear learning through engine", error)),
    }
}

pub fn clear_offline(path: &Path) -> io::Result<ClearRoute> {
    let service = LearningService::open(path)?;
    let cleared_records = service.clear()?;
    Ok(ClearRoute::Offline { cleared_records })
}

fn fault(action: &str, error: Fault) -> io::Error {
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

fn installed_root_policy() -> io::Result<ServerTrustPolicy> {
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
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

    fn temporary_directory(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "sakura-settings-learning-{}-{name}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn offline_clear_replaces_live_and_durable_history() {
        let directory = temporary_directory("clear");
        let path = directory.join("learning.bin");
        let service = LearningService::open(&path).expect("open");
        service.learn("さくら", "桜", 1, 2);
        service.maintain().expect("flush");
        drop(service);
        assert_eq!(view(&path).expect("before").records.len(), 1);
        assert_eq!(
            clear_offline(&path).expect("clear"),
            ClearRoute::Offline { cleared_records: 1 }
        );
        assert!(view(&path).expect("after").records.is_empty());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn missing_learning_file_views_and_exports_as_empty_current_format() {
        let directory = temporary_directory("missing");
        let source = directory.join("missing.bin");
        let destination = directory.join("learning.tsv");
        let snapshot = view(&source).expect("empty view");
        assert_eq!(snapshot.format_version, LEARNING_FORMAT_VERSION);
        assert!(snapshot.records.is_empty());
        assert_eq!(export(&source, &destination).expect("export"), 0);
        assert!(fs::read_to_string(&destination)
            .expect("TSV")
            .contains("sequence\tday"));
        let _ = fs::remove_dir_all(directory);
    }
}
