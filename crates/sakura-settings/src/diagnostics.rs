//! Settings-facing presentation of the bounded IPC diagnostics logs.
//!
//! Timeouts and link resets are rendered by the same code but never merged.
//! They answer different questions — a timeout is a deadline that expired, a
//! reset is a session that was thrown away — and most resets are the correct
//! response to a host lifecycle event, so adding them to the timeout totals
//! would change what those totals have meant since they were introduced.

use std::io;
use std::path::Path;

use sakura_ipc::diagnostics::{
    clear_disconnect_diagnostics, clear_timeout_diagnostics, read_disconnect_diagnostics,
    read_timeout_diagnostics, DisconnectDiagnostics, DisconnectReason, TimeoutDiagnostics,
    TimeoutOperation,
};

pub fn load_timeouts(path: &Path) -> io::Result<TimeoutDiagnostics> {
    read_timeout_diagnostics(path)
}

pub fn clear_timeouts(path: &Path) -> io::Result<()> {
    clear_timeout_diagnostics(path)
}

pub fn load_disconnects(path: &Path) -> io::Result<DisconnectDiagnostics> {
    read_disconnect_diagnostics(path)
}

pub fn clear_disconnects(path: &Path) -> io::Result<()> {
    clear_disconnect_diagnostics(path)
}

pub fn render_text(diagnostics: &TimeoutDiagnostics) -> String {
    let mut output = format!(
        "IPC timeouts: {} valid, {} invalid, {} trailing bytes\n",
        diagnostics.valid_events, diagnostics.invalid_records, diagnostics.ignored_tail_bytes
    );
    for operation in TimeoutOperation::ALL {
        output.push_str(&format!(
            "  {:<16} {}\n",
            operation.name(),
            diagnostics.count(operation)
        ));
    }
    output.push_str(&format!(
        "First timestamp (Unix ms): {}\nLast timestamp (Unix ms): {}\nCapacity reached: {}\n",
        optional_timestamp(diagnostics.first_timestamp_ms),
        optional_timestamp(diagnostics.last_timestamp_ms),
        diagnostics.reached_capacity
    ));
    output
}

pub fn render_tsv(diagnostics: &TimeoutDiagnostics) -> String {
    let mut output = "operation\ttimeout-count\n".to_owned();
    for operation in TimeoutOperation::ALL {
        output.push_str(operation.name());
        output.push('\t');
        output.push_str(&diagnostics.count(operation).to_string());
        output.push('\n');
    }
    output.push_str(&format!(
        "# valid-events: {}\n# invalid-records: {}\n# ignored-tail-bytes: {}\n# first-timestamp-ms: {}\n# last-timestamp-ms: {}\n# reached-capacity: {}\n",
        diagnostics.valid_events,
        diagnostics.invalid_records,
        diagnostics.ignored_tail_bytes,
        optional_timestamp(diagnostics.first_timestamp_ms),
        optional_timestamp(diagnostics.last_timestamp_ms),
        diagnostics.reached_capacity
    ));
    output
}

/// Every reason is printed, including the ones sitting at zero. A reset path
/// that never fires is an answer, and hiding it would leave the reader unable
/// to tell "never happened" from "not measured".
pub fn render_disconnects_text(diagnostics: &DisconnectDiagnostics) -> String {
    let mut output = format!(
        "Engine link resets: {} valid, {} invalid, {} trailing bytes\n",
        diagnostics.valid_events, diagnostics.invalid_records, diagnostics.ignored_tail_bytes
    );
    for reason in DisconnectReason::ALL {
        output.push_str(&format!(
            "  {:<34} {}\n",
            reason.name(),
            diagnostics.count(reason)
        ));
    }
    output.push_str(&format!(
        "First timestamp (Unix ms): {}\nLast timestamp (Unix ms): {}\nCapacity reached: {}\n",
        optional_timestamp(diagnostics.first_timestamp_ms),
        optional_timestamp(diagnostics.last_timestamp_ms),
        diagnostics.reached_capacity
    ));
    output
}

pub fn render_disconnects_tsv(diagnostics: &DisconnectDiagnostics) -> String {
    let mut output = "reason\tdisconnect-count\n".to_owned();
    for reason in DisconnectReason::ALL {
        output.push_str(reason.name());
        output.push('\t');
        output.push_str(&diagnostics.count(reason).to_string());
        output.push('\n');
    }
    output.push_str(&format!(
        "# valid-events: {}\n# invalid-records: {}\n# ignored-tail-bytes: {}\n# first-timestamp-ms: {}\n# last-timestamp-ms: {}\n# reached-capacity: {}\n",
        diagnostics.valid_events,
        diagnostics.invalid_records,
        diagnostics.ignored_tail_bytes,
        optional_timestamp(diagnostics.first_timestamp_ms),
        optional_timestamp(diagnostics.last_timestamp_ms),
        diagnostics.reached_capacity
    ));
    output
}

fn optional_timestamp(timestamp: Option<u64>) -> String {
    timestamp.map_or_else(|| "none".to_owned(), |timestamp| timestamp.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_ipc::diagnostics::{record_disconnect_at, record_timeout_at, TimeoutOperation};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

    fn temporary_file(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "sakura-settings-diagnostics-{}-{name}-{}.bin",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn renders_every_counter_and_clear_is_observable() {
        let path = temporary_file("timeouts");
        record_timeout_at(&path, TimeoutOperation::Key).expect("fixture");
        let diagnostics = load_timeouts(&path).expect("load");
        let text = render_text(&diagnostics);
        let tsv = render_tsv(&diagnostics);
        assert!(text.contains("key              1"));
        assert!(tsv.contains("key\t1\n"));
        for operation in TimeoutOperation::ALL {
            assert!(tsv.contains(operation.name()));
        }
        clear_timeouts(&path).expect("clear");
        assert_eq!(load_timeouts(&path).expect("cleared").valid_events, 0);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn renders_every_reset_reason_and_clear_is_observable() {
        let path = temporary_file("disconnects");
        record_disconnect_at(&path, DisconnectReason::KeyPredecessorFailed).expect("fixture");
        let diagnostics = load_disconnects(&path).expect("load");
        let text = render_disconnects_text(&diagnostics);
        let tsv = render_disconnects_tsv(&diagnostics);
        assert!(text.contains("key-predecessor-failed"));
        assert!(tsv.contains("key-predecessor-failed\t1\n"));
        // A reason at zero must still be listed, or the reader cannot tell a
        // path that never ran from one that was never measured.
        assert!(tsv.contains("service-detached\t0\n"));
        for reason in DisconnectReason::ALL {
            assert!(tsv.contains(reason.name()), "{}", reason.name());
        }
        clear_disconnects(&path).expect("clear");
        assert_eq!(load_disconnects(&path).expect("cleared").valid_events, 0);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn a_reset_is_never_reported_as_a_timeout() {
        // The two logs live in different files, but nothing stops a caller
        // pointing the wrong reader at one of them.
        let path = temporary_file("crossed");
        record_disconnect_at(&path, DisconnectReason::ServiceDetached).expect("fixture");
        let as_timeouts = load_timeouts(&path).expect("load");
        assert_eq!(as_timeouts.valid_events, 0);
        assert!(render_text(&as_timeouts).contains("IPC timeouts: 0 valid, 1 invalid"));
        let _ = fs::remove_file(path);
    }
}
