//! Asks the running engine whether any artificial delay is armed in it.
//!
//! The engine ships with four deliberate delay points so a stress harness can
//! hold a real engine executable still at a named moment (see the engine
//! fault-injection module). They can only be armed by a command-line
//! argument that the engine accepts solely alongside a private test pipe, so a
//! user's engine cannot have them on. This module exists so that claim is
//! something a user can check rather than something they have to take on
//! trust: it reads the four counters back out of whatever engine is actually
//! answering this session's pipe.
//!
//! Nothing here is content. Each row is a fixed point name, a configured
//! delay and two counters.

use std::io;

use sakura_ipc::diagnostics::{record_timeout, TimeoutOperation};
use sakura_ipc::Fault;
use sakura_proto::{FaultInjectionEntry, FaultPoint, Request, Response};

use crate::engine_admin::{fault, ConnectError, ADMIN_CALL_BUDGET};

/// What one reading found, or that no engine was running to ask.
#[derive(Debug)]
pub enum FaultSnapshot {
    /// The engine answered. Entries are in `FaultPoint::ALL` order.
    Live(Vec<FaultInjectionEntry>),
    /// Windows proved there is no engine pipe. Reported rather than turned
    /// into an error, because a missing engine is a complete answer to the
    /// question and must not read back as "nothing is armed".
    EngineNotRunning,
}

pub fn read() -> io::Result<FaultSnapshot> {
    let mut client = match crate::engine_admin::connect() {
        Ok(client) => client,
        Err(ConnectError::Absent) => return Ok(FaultSnapshot::EngineNotRunning),
        Err(ConnectError::Failed(error)) => return Err(error),
    };
    match client.call(&Request::FaultStatus, ADMIN_CALL_BUDGET) {
        Ok(Response::FaultStatus { entries }) => Ok(FaultSnapshot::Live(entries)),
        Ok(Response::Error(code)) => Err(io::Error::other(format!(
            "engine could not read its fault-injection status: {code:?}"
        ))),
        Ok(response) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected fault-injection response: {response:?}"),
        )),
        Err(Fault::Timeout) => {
            let _ = record_timeout(TimeoutOperation::Administration, client.last_call_elapsed());
            Err(fault(
                "read fault-injection status through engine",
                Fault::Timeout,
            ))
        }
        Err(error) => Err(fault("read fault-injection status through engine", error)),
    }
}

/// True when the engine that answered has nothing armed at any point. This is
/// the shape a user's engine must always have, and the shape a stress run must
/// *not* have while it believes it is injecting.
pub fn is_disarmed(entries: &[FaultInjectionEntry]) -> bool {
    entries.iter().all(|entry| !entry.is_armed())
}

pub fn render_text(snapshot: &FaultSnapshot) -> String {
    let entries = match snapshot {
        FaultSnapshot::EngineNotRunning => {
            return "Fault injection: no engine is running\n".to_owned()
        }
        FaultSnapshot::Live(entries) => entries,
    };
    let mut output = if is_disarmed(entries) {
        "Fault injection: nothing armed in the running engine\n".to_owned()
    } else {
        "Fault injection: ARMED in the running engine\n".to_owned()
    };
    for entry in entries {
        output.push_str(&format!(
            "  {:<18} {:<24} fired {}, slept {} us\n",
            entry.point.name(),
            describe(entry),
            entry.fired,
            entry.slept_us
        ));
    }
    output
}

pub fn render_tsv(snapshot: &FaultSnapshot) -> String {
    let entries = match snapshot {
        FaultSnapshot::EngineNotRunning => return "# engine-running: false\n".to_owned(),
        FaultSnapshot::Live(entries) => entries,
    };
    let mut output = "point\tdelay-us\tremaining\tfired\tslept-us\n".to_owned();
    for entry in entries {
        output.push_str(entry.point.name());
        output.push('\t');
        output.push_str(&entry.delay_us.to_string());
        output.push('\t');
        // "every time" is unbounded, not a large number, and printing
        // `u64::MAX` would invite arithmetic on it.
        output.push_str(&if entry.remaining == u64::MAX {
            "every".to_owned()
        } else {
            entry.remaining.to_string()
        });
        output.push('\t');
        output.push_str(&entry.fired.to_string());
        output.push('\t');
        output.push_str(&entry.slept_us.to_string());
        output.push('\n');
    }
    output.push_str(&format!("# armed: {}\n", !is_disarmed(entries)));
    output.push_str("# engine-running: true\n");
    output
}

fn describe(entry: &FaultInjectionEntry) -> String {
    if !entry.is_armed() {
        return "disarmed".to_owned();
    }
    let remaining = if entry.remaining == u64::MAX {
        "every".to_owned()
    } else {
        format!("{} left", entry.remaining)
    };
    format!("{} us, {remaining}", entry.delay_us)
}

/// A reading with a point missing, or with two of the same point, is a broken
/// reading rather than a partial one. Accepting it would let a stress run read
/// one point's counters under another point's name.
pub fn names_every_point_once(entries: &[FaultInjectionEntry]) -> bool {
    entries.len() == FaultPoint::ALL.len()
        && entries
            .iter()
            .zip(FaultPoint::ALL)
            .all(|(entry, point)| entry.point == point)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disarmed() -> Vec<FaultInjectionEntry> {
        FaultPoint::ALL
            .iter()
            .map(|point| FaultInjectionEntry {
                point: *point,
                delay_us: 0,
                remaining: 0,
                fired: 0,
                slept_us: 0,
            })
            .collect()
    }

    #[test]
    fn a_production_engine_reads_back_as_disarmed_at_every_point() {
        let entries = disarmed();
        assert!(is_disarmed(&entries));
        let text = render_text(&FaultSnapshot::Live(entries.clone()));
        assert!(text.contains("nothing armed"));
        for point in FaultPoint::ALL {
            assert!(text.contains(point.name()), "{} is missing", point.name());
        }
        let tsv = render_tsv(&FaultSnapshot::Live(entries));
        assert!(tsv.contains("before-dispatch\t0\t0\t0\t0\n"));
        assert!(tsv.contains("# armed: false\n"));
    }

    #[test]
    fn an_armed_point_is_named_with_what_it_actually_did() {
        let mut entries = disarmed();
        entries[FaultPoint::AfterMutation as usize] = FaultInjectionEntry {
            point: FaultPoint::AfterMutation,
            delay_us: 50_000,
            remaining: u64::MAX,
            fired: 3,
            slept_us: 154_212,
        };
        assert!(!is_disarmed(&entries));
        let text = render_text(&FaultSnapshot::Live(entries.clone()));
        assert!(text.contains("ARMED"));
        assert!(text.contains("after-mutation"));
        // Measured, not requested: a loaded scheduler oversleeps, and the
        // number a harness asserts on has to be the one that happened.
        assert!(text.contains("fired 3, slept 154212 us"));

        let tsv = render_tsv(&FaultSnapshot::Live(entries));
        assert!(tsv.contains("after-mutation\t50000\tevery\t3\t154212\n"));
        assert!(tsv.contains("# armed: true\n"));
    }

    /// A missing engine must not be renderable as a disarmed one: the two
    /// answers differ in whether anything was checked at all.
    #[test]
    fn an_absent_engine_is_reported_and_not_rendered_as_disarmed() {
        let text = render_text(&FaultSnapshot::EngineNotRunning);
        assert!(text.contains("no engine is running"));
        assert!(!text.contains("before-dispatch"));
        assert_eq!(
            render_tsv(&FaultSnapshot::EngineNotRunning),
            "# engine-running: false\n"
        );
    }

    #[test]
    fn a_short_or_reordered_reading_is_not_accepted() {
        assert!(names_every_point_once(&disarmed()));

        let mut short = disarmed();
        short.pop();
        assert!(!names_every_point_once(&short));

        let mut swapped = disarmed();
        swapped.swap(0, 1);
        assert!(!names_every_point_once(&swapped));
    }

    /// A delay of zero occurrences, or zero microseconds, cannot fire. Both
    /// have to read as disarmed, or a harness could believe a point was live
    /// because it had a number in one column.
    #[test]
    fn a_point_that_cannot_fire_counts_as_disarmed() {
        let mut entries = disarmed();
        entries[FaultPoint::BeforeDispatch as usize].delay_us = 50_000;
        entries[FaultPoint::DuringReply as usize].remaining = 4;
        assert!(is_disarmed(&entries));
    }
}
