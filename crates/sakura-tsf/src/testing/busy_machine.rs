//! Scenarios about a reply that has to arrive inside a budget of tens of
//! milliseconds, on a machine that may be busy.
//!
//! The tests of the keystroke thread's budgets (10 ms for placement, 50 ms
//! design, 200 ms patient) put a scripted peer on the other end of a pipe and
//! ask whether its reply was in time. On an idle machine it always is. On a
//! busy one the scheduler can hold the peer, or the test thread, past the
//! budget for reasons that have nothing to do with the code under test, and
//! the product budget is not ours to widen to make room for it.
//! [`until_the_machine_keeps_up`] gives such a scenario a few runs instead.
#![allow(clippy::expect_used, clippy::panic)]

use std::time::Duration;

/// How many times a scenario runs before the test gives up.
const ATTEMPTS: u32 = 5;

/// The pause after the first miss. It doubles after each further one, so a
/// burst of load that outlasts one run is not met again by the next.
const FIRST_PAUSE: Duration = Duration::from_millis(50);

/// Runs `attempt` until it returns `Ok`, at most [`ATTEMPTS`] times, and fails
/// the test if it never does.
///
/// The contract with the scenario is what keeps this from hiding bugs:
/// `attempt` returns `Err` only for a check that a busy machine can fail (a
/// reply that missed a short budget, an elapsed time with an upper bound), and
/// asserts everything else outright, including every lower bound (load only
/// makes a wait longer). An implementation with the wrong budget then fails
/// every run, and a busy machine only some, which is the difference these
/// tests exist to show. The scenario builds all of its own state, because it
/// runs more than once.
pub(crate) fn until_the_machine_keeps_up(
    scenario: &str,
    attempt: impl FnMut() -> Result<(), String>,
) {
    retrying(scenario, ATTEMPTS, FIRST_PAUSE, attempt);
}

fn retrying(
    scenario: &str,
    attempts: u32,
    first_pause: Duration,
    mut attempt: impl FnMut() -> Result<(), String>,
) {
    let mut misses = Vec::new();
    for round in 0..attempts {
        match attempt() {
            Ok(()) => return,
            Err(miss) => misses.push(miss),
        }
        if round + 1 < attempts {
            std::thread::sleep(first_pause * (1 << round));
        }
    }
    panic!("{scenario} missed its window in all {attempts} attempts: {misses:#?}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::panic::{catch_unwind, AssertUnwindSafe};
    use std::time::Instant;

    #[test]
    fn a_scenario_that_passes_runs_once() {
        let runs = Cell::new(0);
        retrying("passes", 3, Duration::ZERO, || {
            runs.set(runs.get() + 1);
            Ok(())
        });
        assert_eq!(runs.get(), 1);
    }

    #[test]
    fn a_scenario_that_misses_runs_again_until_it_passes() {
        let runs = Cell::new(0);
        retrying("misses twice", 5, Duration::ZERO, || {
            runs.set(runs.get() + 1);
            if runs.get() < 3 {
                Err(format!("miss {}", runs.get()))
            } else {
                Ok(())
            }
        });
        assert_eq!(runs.get(), 3);
    }

    #[test]
    fn a_scenario_that_never_passes_fails_the_test_and_names_every_miss() {
        let runs = Cell::new(0);
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            retrying("never passes", 3, Duration::ZERO, || {
                runs.set(runs.get() + 1);
                Err(format!("miss {}", runs.get()))
            });
        }));
        let payload = outcome.expect_err("a scenario that always misses must fail");
        let message = payload
            .downcast_ref::<String>()
            .expect("a formatted panic message");
        assert_eq!(runs.get(), 3);
        for needle in [
            "never passes",
            "all 3 attempts",
            "miss 1",
            "miss 2",
            "miss 3",
        ] {
            assert!(
                message.contains(needle),
                "{needle:?} missing from {message}"
            );
        }
    }

    #[test]
    fn an_assertion_failing_inside_the_scenario_is_not_run_again() {
        let runs = Cell::new(0);
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            retrying("asserts", 5, Duration::ZERO, || {
                runs.set(runs.get() + 1);
                assert_eq!(runs.get(), 0, "a wrong implementation fails outright");
                Ok(())
            });
        }));
        assert!(outcome.is_err());
        assert_eq!(runs.get(), 1);
    }

    #[test]
    fn each_miss_is_followed_by_a_longer_pause() {
        let started = Instant::now();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            retrying("pauses", 3, Duration::from_millis(20), || {
                Err("miss".to_owned())
            });
        }));
        let waited = started.elapsed();
        assert!(outcome.is_err());
        // 20 ms after the first miss and 40 ms after the second. A busy
        // machine can only lengthen a sleep, so this lower bound holds under
        // any load.
        assert!(waited >= Duration::from_millis(60), "{waited:?}");
    }
}
