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
}
