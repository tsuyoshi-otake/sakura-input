//! How long a key callback waits for the engine, and when it stops waiting
//! that long (#415).
//!
//! DESIGN 4.3 gave every keystroke 50 ms. A reply that missed it cost the
//! keystroke and, because the link is then resynchronized, the reading the
//! user had on screen as well (#148). The owner's decision of 2026-10-28 is
//! that a wait of up to 200 ms is acceptable and a dropped composition is
//! not, so a key callback waits [`PATIENT_BUDGET`] for as long as the engine
//! keeps answering.
//!
//! An engine that has stopped answering must not hold the host's UI thread
//! for 200 ms on every key. Two waits in a row that end without a reply
//! therefore return the callbacks to [`DESIGN_BUDGET`], and the first reply to
//! any request restores the long wait:
//!
//! | state                | a callback waits    | after a miss        | after a reply |
//! |----------------------|---------------------|---------------------|---------------|
//! | patient, no misses   | [`PATIENT_BUDGET`]  | patient, one miss   | unchanged     |
//! | patient, one miss    | [`PATIENT_BUDGET`]  | fallback            | patient       |
//! | fallback             | [`DESIGN_BUDGET`]   | fallback            | patient       |
//!
//! A miss is a request that was issued and whose reply did not arrive by the
//! callback's deadline. A request that was never issued because the deadline
//! had already passed, and a link that broke, say nothing about whether the
//! engine answers, so they change nothing. One key press can use up both
//! misses: the host calls `OnTestKeyDown` and `OnKeyDown` for it, and each is
//! a callback of its own.
//!
//! The count is per thread, like [`crate::callback_deadline`]'s deadline: both
//! describe the host's keystroke thread, and the callback needs the budget
//! before it has touched the engine, whose state sits behind a `RefCell` that a
//! re-entrant callback may already hold. A thread-local `Cell` needs neither.

use std::cell::Cell;
use std::time::Duration;

use crate::callback_deadline::CallbackDeadline;

/// DESIGN 4.3's per-keystroke budget. It is still what a request made outside
/// a key callback may wait, and what a key callback waits once the engine has
/// missed two waits in a row.
pub(crate) const DESIGN_BUDGET: Duration = Duration::from_millis(50);

/// The longest a key callback waits while the engine is answering: the owner's
/// ceiling for the key wait (#415).
pub(crate) const PATIENT_BUDGET: Duration = Duration::from_millis(200);

/// Consecutive misses after which callbacks wait only [`DESIGN_BUDGET`].
const MISSES_BEFORE_FALLBACK: u8 = 2;

thread_local! {
    static CONSECUTIVE_MISSES: Cell<u8> = const { Cell::new(0) };
}

/// The allowance the next key callback on this thread enters with.
pub(crate) fn budget() -> Duration {
    if CONSECUTIVE_MISSES.with(Cell::get) >= MISSES_BEFORE_FALLBACK {
        DESIGN_BUDGET
    } else {
        PATIENT_BUDGET
    }
}

/// Opens the IPC allowance of one key callback (`OnTestKeyDown`, `OnKeyDown`,
/// `OnPreservedKey`) for as long as the guard lives. Every key entry point goes
/// through here, so none of them needs to know the budget.
pub(crate) fn enter_key_callback() -> CallbackDeadline {
    CallbackDeadline::enter(budget())
}

/// The engine answered a request made under the key allowance. What the answer
/// says does not matter here; only that the engine is replying.
pub(crate) fn record_answer() {
    CONSECUTIVE_MISSES.with(|misses| misses.set(0));
}

/// A request made under the key allowance was issued and its reply did not
/// arrive in time.
pub(crate) fn record_miss() {
    CONSECUTIVE_MISSES.with(|misses| {
        misses.set(misses.get().saturating_add(1).min(MISSES_BEFORE_FALLBACK));
    });
}

/// How many waits in a row have ended without a reply. The tests read it to
/// tell "no miss was recorded" from "one was", which [`budget`] cannot: a
/// single miss leaves the budget long.
#[cfg(test)]
pub(crate) fn consecutive_misses() -> u8 {
    CONSECUTIVE_MISSES.with(Cell::get)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every test runs on a thread of its own, but nothing here relies on that.
    fn fresh() {
        record_answer();
    }

    #[test]
    fn the_budgets_are_the_owners_numbers() {
        assert_eq!(DESIGN_BUDGET, Duration::from_millis(50));
        assert_eq!(PATIENT_BUDGET, Duration::from_millis(200));
        assert_eq!(MISSES_BEFORE_FALLBACK, 2);
    }

    #[test]
    fn the_wait_starts_patient() {
        fresh();
        assert_eq!(budget(), PATIENT_BUDGET);
    }

    #[test]
    fn one_miss_keeps_the_long_wait_and_the_second_ends_it() {
        fresh();
        record_miss();
        assert_eq!(budget(), PATIENT_BUDGET);
        record_miss();
        assert_eq!(budget(), DESIGN_BUDGET);
    }

    #[test]
    fn further_misses_stay_at_the_design_budget() {
        fresh();
        for _ in 0..10 {
            record_miss();
        }
        assert_eq!(budget(), DESIGN_BUDGET);
        // Three states only (no miss, one miss, fallen back): further misses
        // do not deepen the fall-back, as the table above says.
        assert_eq!(consecutive_misses(), MISSES_BEFORE_FALLBACK);
    }

    #[test]
    fn an_answer_restores_the_long_wait_however_many_misses_came_first() {
        fresh();
        for _ in 0..10 {
            record_miss();
        }
        record_answer();
        assert_eq!(budget(), PATIENT_BUDGET);
        // The count restarted: one miss is not yet a fallback.
        record_miss();
        assert_eq!(budget(), PATIENT_BUDGET);
    }

    #[test]
    fn misses_with_an_answer_between_them_are_not_consecutive() {
        fresh();
        record_miss();
        record_answer();
        record_miss();
        assert_eq!(budget(), PATIENT_BUDGET);
    }

    #[test]
    fn a_key_callback_enters_with_the_budget_the_policy_gives_the_next_key() {
        use crate::callback_deadline::remaining_or;
        use std::time::Instant;

        fresh();
        let before = Instant::now();
        {
            let _patient = enter_key_callback();
            let deadline = remaining_or(DESIGN_BUDGET);
            assert!(deadline >= before + PATIENT_BUDGET);
            assert!(deadline <= Instant::now() + PATIENT_BUDGET);
        }
        record_miss();
        record_miss();
        let before = Instant::now();
        {
            let _fallen_back = enter_key_callback();
            let deadline = remaining_or(PATIENT_BUDGET);
            assert!(deadline >= before + DESIGN_BUDGET);
            assert!(deadline <= Instant::now() + DESIGN_BUDGET);
        }
    }

    /// The policy takes effect only where a key callback enters its deadline,
    /// and no unit test can drive those COM entry points. Pin the wiring by
    /// reading it: the three key callbacks enter through [`enter_key_callback`]
    /// and nothing in the text service opens a deadline of its own.
    #[test]
    fn the_text_services_key_callbacks_enter_through_the_policy() {
        let source = include_str!("text_service.rs");
        assert_eq!(
            source.matches("CallbackDeadline::enter(").count(),
            0,
            "a key entry point chose its own budget instead of key_wait's"
        );
        assert_eq!(
            source.matches("key_wait::enter_key_callback()").count(),
            3,
            "OnTestKeyDown, OnKeyDown and OnPreservedKey each enter once"
        );
    }

    #[test]
    fn each_thread_counts_its_own_misses() {
        fresh();
        record_miss();
        record_miss();
        assert_eq!(budget(), DESIGN_BUDGET);
        let other = std::thread::spawn(budget).join().ok();
        assert_eq!(other, Some(PATIENT_BUDGET));
        assert_eq!(budget(), DESIGN_BUDGET);
    }
}
