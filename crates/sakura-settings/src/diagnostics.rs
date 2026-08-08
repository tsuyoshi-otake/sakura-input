//! Settings-facing presentation of bounded IPC timeout diagnostics.

use std::io;
use std::path::Path;

use sakura_ipc::diagnostics::{
    clear_timeout_diagnostics, read_timeout_diagnostics, TimeoutDiagnostics, TimeoutOperation,
};

pub fn load(path: &Path) -> io::Result<TimeoutDiagnostics> {
    read_timeout_diagnostics(path)
}

pub fn clear(path: &Path) -> io::Result<()> {
    clear_timeout_diagnostics(path)
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

fn optional_timestamp(timestamp: Option<u64>) -> String {
    timestamp.map_or_else(|| "none".to_owned(), |timestamp| timestamp.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_ipc::diagnostics::{record_timeout_at, TimeoutOperation};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn renders_every_counter_and_clear_is_observable() {
        let path = std::env::temp_dir().join(format!(
            "sakura-settings-diagnostics-{}-{}.bin",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        record_timeout_at(&path, TimeoutOperation::Key).expect("fixture");
        let diagnostics = load(&path).expect("load");
        let text = render_text(&diagnostics);
        let tsv = render_tsv(&diagnostics);
        assert!(text.contains("key              1"));
        assert!(tsv.contains("key\t1\n"));
        for operation in TimeoutOperation::ALL {
            assert!(tsv.contains(operation.name()));
        }
        clear(&path).expect("clear");
        assert_eq!(load(&path).expect("cleared").valid_events, 0);
        let _ = fs::remove_file(path);
    }
}
