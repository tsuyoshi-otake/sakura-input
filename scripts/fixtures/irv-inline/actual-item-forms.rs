#[cfg(test)]
use std::cell::Cell;

#[cfg(test)]
struct TestCounts {
    value: usize,
}

#[cfg(test)]
impl TestCounts {
    const fn new() -> Self {
        Self { value: 0 }
    }
}

#[cfg(test)]
std::thread_local! {
    static CALLS: Cell<usize> = const { Cell::new(0) };
}

struct Production {
    #[cfg(test)]
    test_only: Option<Vec<u8>>,
    live: bool,
}

fn construct() -> Production {
    Production {
        #[cfg(test)]
        test_only: None,
        live: true,
    }
}

fn statement_forms() {
    #[cfg(test)]
    record_for_test();
    #[cfg(test)]
    if 1 < 2 {
        record_for_test();
    }
}

#[cfg(test)]
const fn helper() -> usize {
    1
}

#[cfg(test)]
#[path = "unit/tests.rs"]
mod tests;
fn production_after() {}
