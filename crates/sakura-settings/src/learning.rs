//! Learning viewer, exporter, and race-safe clear operation.

use std::io;
use std::path::Path;

use sakura_ipc::diagnostics::{record_timeout, TimeoutOperation};
use sakura_ipc::{Client, Endpoint, Fault};
use sakura_proto::{Request, Response};
#[cfg(test)]
use sakura_store::learning::LearningRecord;
use sakura_store::learning::{
    read_snapshot, LearningLog, LearningSnapshot, LEARNING_FORMAT_VERSION,
};

use crate::engine_admin::{
    engine_is_definitely_absent, fault, handshake, installed_root_policy, ADMIN_CALL_BUDGET,
};
use crate::storage::atomic_write;

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
    atomic_write(destination, snapshot_to_tsv(&snapshot).as_bytes())?;
    Ok(snapshot.records.len())
}

/// Stable, UTF-8 TSV presentation owned by the settings surface. Text fields
/// use backslash escaping because a committed surface may contain controls.
pub fn snapshot_to_tsv(snapshot: &LearningSnapshot) -> String {
    let mut output = format!(
        "# sakura-learning-format: {}\nsequence\tday\tleft-context\tright-context\treading\tsurface\n",
        snapshot.format_version
    );
    for record in &snapshot.records {
        output.push_str(&record.sequence.to_string());
        output.push('\t');
        output.push_str(&record.day.to_string());
        output.push('\t');
        output.push_str(&record.left_context.to_string());
        output.push('\t');
        output.push_str(&record.right_context.to_string());
        output.push('\t');
        push_tsv_escaped(&mut output, &record.reading);
        output.push('\t');
        push_tsv_escaped(&mut output, &record.surface);
        output.push('\n');
    }
    output
}

fn push_tsv_escaped(output: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '\\' => output.push_str("\\\\"),
            '\t' => output.push_str("\\t"),
            '\r' => output.push_str("\\r"),
            '\n' => output.push_str("\\n"),
            other => output.push(other),
        }
    }
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
    handshake(&mut client)?;

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
            let _ = record_timeout(TimeoutOperation::Administration, client.last_call_elapsed());
            Err(fault("clear learning through engine", Fault::Timeout))
        }
        Err(error) => Err(fault("clear learning through engine", error)),
    }
}

pub fn clear_offline(path: &Path) -> io::Result<ClearRoute> {
    let (mut log, _, _open_receipt) =
        LearningLog::open(path, |_| ()).map_err(|error| error.source)?;
    let (cleared_records, _, _clear_receipt) = log.clear(|_| ()).map_err(|error| error.source)?;
    Ok(ClearRoute::Offline { cleared_records })
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
        let (mut log, _, _) = LearningLog::open(&path, |_| ()).expect("open");
        log.append("さくら", "桜", 1, 2, 20708).expect("append");
        drop(log);
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

    #[test]
    fn learning_snapshot_tsv_preserves_exact_escaping() {
        let snapshot = LearningSnapshot {
            format_version: LEARNING_FORMAT_VERSION,
            records: vec![LearningRecord {
                sequence: 1,
                day: 2,
                left_context: 7,
                right_context: 9,
                reading: "さく\\ら".to_owned(),
                surface: "Sakura\tInput\r\n".to_owned(),
            }],
            ignored_tail_bytes: 0,
        };
        assert_eq!(
            snapshot_to_tsv(&snapshot),
            "# sakura-learning-format: 3\nsequence\tday\tleft-context\tright-context\treading\tsurface\n1\t2\t7\t9\tさく\\\\ら\tSakura\\tInput\\r\\n\n"
        );
    }
}
