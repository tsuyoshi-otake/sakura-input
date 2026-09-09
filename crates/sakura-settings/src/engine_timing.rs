//! Reads the running engine's per-stage timing accumulators.
//!
//! Unlike the timeout and link-reset logs, this is not a file: the numbers
//! live in the engine process and are gone when it exits. That is deliberate.
//! They are cumulative totals that only mean anything relative to another
//! reading of the same process, and writing them to disk would invite reading
//! one process's totals as if they were another's.
//!
//! Nothing here is content. Each row is a stage name, a count and two
//! durations, so a snapshot can be taken during ordinary use and attached to
//! an issue without disclosing anything that was typed.

use std::io;

use sakura_ipc::diagnostics::{record_timeout, TimeoutOperation};
use sakura_ipc::Fault;
use sakura_proto::{EngineTimingEntry, EngineTimingSite, Request, Response};

use crate::engine_admin::{fault, ConnectError, ADMIN_CALL_BUDGET};

/// What one snapshot found, or that no engine was running to ask.
#[derive(Debug)]
pub enum TimingSnapshot {
    /// The engine answered. Entries are in `EngineTimingSite::ALL` order.
    Live(Vec<EngineTimingEntry>),
    /// Windows proved there is no engine pipe. This is reported rather than
    /// turned into an error because "no engine is running" is a complete
    /// answer to the question, and a stress harness needs to be able to tell
    /// it apart from a call that failed.
    EngineNotRunning,
}

pub fn read() -> io::Result<TimingSnapshot> {
    let mut client = match crate::engine_admin::connect() {
        Ok(client) => client,
        Err(ConnectError::Absent) => return Ok(TimingSnapshot::EngineNotRunning),
        Err(ConnectError::Failed(error)) => return Err(error),
    };
    match client.call(&Request::EngineTiming, ADMIN_CALL_BUDGET) {
        Ok(Response::EngineTiming { entries }) => Ok(TimingSnapshot::Live(entries)),
        Ok(Response::Error(code)) => Err(io::Error::other(format!(
            "engine could not read its timing snapshot: {code:?}"
        ))),
        Ok(response) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected engine timing response: {response:?}"),
        )),
        Err(Fault::Timeout) => {
            let _ = record_timeout(TimeoutOperation::Administration, client.last_call_elapsed());
            Err(fault("read engine timing through engine", Fault::Timeout))
        }
        Err(error) => Err(fault("read engine timing through engine", error)),
    }
}

pub fn render_text(snapshot: &TimingSnapshot) -> String {
    let entries = match snapshot {
        TimingSnapshot::EngineNotRunning => {
            return "Engine timing: no engine is running\n".to_owned()
        }
        TimingSnapshot::Live(entries) => entries,
    };
    let mut output = "Engine timing (cumulative since the engine started)\n".to_owned();
    for entry in entries {
        output.push_str(&format!(
            "  {:<28} {:<10} {}\n",
            entry.site.name(),
            entry.samples,
            describe(entry)
        ));
    }
    output
}

pub fn render_tsv(snapshot: &TimingSnapshot) -> String {
    let entries = match snapshot {
        TimingSnapshot::EngineNotRunning => return "# engine-running: false\n".to_owned(),
        TimingSnapshot::Live(entries) => entries,
    };
    let mut output = "site\tsamples\tmean-us\tmax-us\ttotal-us\n".to_owned();
    for entry in entries {
        output.push_str(entry.site.name());
        output.push('\t');
        output.push_str(&entry.samples.to_string());
        output.push('\t');
        // Empty rather than `0`, so a stage this engine never reached is not
        // read back as one that took no time.
        output.push_str(&entry.mean_us().map_or(String::new(), |us| us.to_string()));
        output.push('\t');
        output.push_str(&if entry.samples > 0 {
            entry.max_us.to_string()
        } else {
            String::new()
        });
        output.push('\t');
        output.push_str(&entry.total_us.to_string());
        output.push('\n');
    }
    output.push_str("# engine-running: true\n");
    output
}

/// Every site is listed even at zero, so a reader can see that a stage exists
/// and was never entered rather than wondering whether it was measured at all.
fn describe(entry: &EngineTimingEntry) -> String {
    match entry.mean_us() {
        Some(mean) => format!("mean {mean} us, max {} us", entry.max_us),
        None => "not reached".to_owned(),
    }
}

/// A snapshot with an entry missing, or with two of the same site, is a broken
/// reading rather than a partial one, and treating it as data would put a
/// stage's numbers under another stage's name.
pub fn names_every_site_once(entries: &[EngineTimingEntry]) -> bool {
    entries.len() == EngineTimingSite::ALL.len()
        && entries
            .iter()
            .zip(EngineTimingSite::ALL)
            .all(|(entry, site)| entry.site == site)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(
        site: EngineTimingSite,
        samples: u64,
        total_us: u64,
        max_us: u64,
    ) -> EngineTimingEntry {
        EngineTimingEntry {
            site,
            samples,
            total_us,
            max_us,
        }
    }

    fn full_snapshot() -> Vec<EngineTimingEntry> {
        EngineTimingSite::ALL
            .iter()
            .map(|site| entry(*site, 0, 0, 0))
            .collect()
    }

    #[test]
    fn a_reached_site_reports_its_mean_and_an_unreached_one_says_so() {
        let mut entries = full_snapshot();
        entries[EngineTimingSite::RuntimeServicesLockWait as usize] =
            entry(EngineTimingSite::RuntimeServicesLockWait, 4, 8_000, 6_000);
        let snapshot = TimingSnapshot::Live(entries);
        let text = render_text(&snapshot);
        assert!(text.contains("runtime-services-lock-wait"));
        assert!(text.contains("mean 2000 us, max 6000 us"));
        // The distinction that matters: a stage nothing ever entered must not
        // be reported as one that was instant.
        assert!(text.contains("not reached"));

        let tsv = render_tsv(&snapshot);
        assert!(tsv.contains("runtime-services-lock-wait\t4\t2000\t6000\t8000\n"));
        assert!(tsv.contains("dispatch\t0\t\t\t0\n"));
        assert!(tsv.contains("# engine-running: true\n"));
    }

    /// "No engine is running" has to survive to the reader as itself. A
    /// stress harness that read it as a table of zeros would record a run in
    /// which nothing was measured as a run in which nothing was slow.
    #[test]
    fn an_absent_engine_is_reported_and_not_rendered_as_zeroes() {
        let text = render_text(&TimingSnapshot::EngineNotRunning);
        assert!(text.contains("no engine is running"));
        assert!(!text.contains("dispatch"));
        assert_eq!(
            render_tsv(&TimingSnapshot::EngineNotRunning),
            "# engine-running: false\n"
        );
    }

    #[test]
    fn a_short_or_reordered_snapshot_is_not_accepted() {
        assert!(names_every_site_once(&full_snapshot()));

        let mut short = full_snapshot();
        short.pop();
        assert!(!names_every_site_once(&short));

        let mut swapped = full_snapshot();
        swapped.swap(0, 1);
        assert!(!names_every_site_once(&swapped));
    }
}
