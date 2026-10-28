//! One IPC allowance for a synchronous TSF callback, including COM reentry.
use std::cell::Cell;
use std::marker::PhantomData;
use std::time::{Duration, Instant};

thread_local! {
    static CURRENT: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Thread-bound scope; nested callbacks inherit the earlier expiry.
#[derive(Debug)]
pub(crate) struct CallbackDeadline {
    previous: Option<Instant>,
    _same_thread: PhantomData<*mut ()>,
}

impl CallbackDeadline {
    pub(crate) fn enter(budget: Duration) -> Self {
        let deadline = limit(budget);
        Self {
            previous: CURRENT.with(|current| current.replace(Some(deadline))),
            _same_thread: PhantomData,
        }
    }
}

impl Drop for CallbackDeadline {
    fn drop(&mut self) {
        CURRENT.with(|current| current.set(self.previous));
    }
}

pub(crate) fn limit(budget: Duration) -> Instant {
    let own = Instant::now() + budget;
    CURRENT.with(|current| current.get().map_or(own, |parent| parent.min(own)))
}

/// The deadline of a request that has no cap of its own: the active
/// callback's whole remaining allowance, or `standalone` from now when no
/// callback owns the thread. Unlike [`limit`] it never shortens the callback's
/// allowance, so a request inside a long callback is not cut off at the
/// length a request outside one gets.
pub(crate) fn remaining_or(standalone: Duration) -> Instant {
    CURRENT.with(|current| current.get().unwrap_or_else(|| Instant::now() + standalone))
}

/// Whether the active callback has spent its whole allowance. Nothing expires
/// outside a callback.
pub(crate) fn expired() -> bool {
    CURRENT.with(|current| {
        current
            .get()
            .is_some_and(|deadline| Instant::now() >= deadline)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_deadline_nested_scope_restores_parent_without_extension() {
        assert_eq!(CURRENT.with(Cell::get), None);
        {
            let _outer = CallbackDeadline::enter(Duration::from_secs(1));
            let outer = CURRENT.with(Cell::get);
            {
                let _inner = CallbackDeadline::enter(Duration::from_secs(2));
                assert_eq!(CURRENT.with(Cell::get), outer);
            }
            assert_eq!(CURRENT.with(Cell::get), outer);
            {
                let _expired = CallbackDeadline::enter(Duration::ZERO);
                assert!(limit(Duration::from_secs(2)) <= Instant::now());
                let _nested = CallbackDeadline::enter(Duration::from_secs(2));
                assert!(limit(Duration::from_secs(2)) <= Instant::now());
            }
            assert_eq!(CURRENT.with(Cell::get), outer);
        }
        assert_eq!(CURRENT.with(Cell::get), None);
    }

    #[test]
    fn a_request_inside_a_callback_gets_its_whole_remaining_allowance() {
        let standalone = Duration::from_millis(50);
        let before = Instant::now();
        assert!(
            remaining_or(standalone) >= before + standalone,
            "outside a callback the request gets the standalone allowance"
        );
        {
            let _long = CallbackDeadline::enter(Duration::from_secs(1));
            let deadline = remaining_or(standalone);
            assert!(
                deadline >= before + Duration::from_secs(1),
                "a long callback was cut to the standalone length"
            );
            assert_eq!(Some(deadline), CURRENT.with(Cell::get));
        }
        {
            let _short = CallbackDeadline::enter(Duration::from_millis(10));
            assert!(
                remaining_or(Duration::from_secs(5)) <= Instant::now() + Duration::from_millis(10),
                "a short callback was extended to the standalone length"
            );
        }
        assert!(remaining_or(standalone) >= before + standalone);
    }

    #[test]
    fn expiry_is_reported_only_inside_a_spent_callback() {
        assert!(!expired());
        {
            let _live = CallbackDeadline::enter(Duration::from_secs(1));
            assert!(!expired());
        }
        {
            let _spent = CallbackDeadline::enter(Duration::ZERO);
            assert!(expired());
        }
        assert!(!expired());
    }
}
