//! Process-wide, content-free accumulators for how long each stage of a
//! request took.
//!
//! The high-load work in #148 has to separate "the engine was busy converting"
//! from "the engine was waiting behind a lock somebody else held" from "the
//! engine answered quickly and the reply was late anyway". Counting timeouts
//! cannot do that, and neither can a stack trace taken after the fact, because
//! the interesting events are rare and are gone by the time anyone looks.
//!
//! So the measurement is always on. Every site is three relaxed atomics in a
//! fixed-size table; observing one is two `Instant::now()` calls and three
//! atomic updates, with no allocation, no lock, and no growth over the life of
//! the process. That cost is paid on every request rather than only when a
//! diagnostic mode is enabled, which is the point: a stall nobody predicted
//! must already have been recorded when it happens, and a diagnostic that has
//! to be switched on first cannot record the first occurrence of anything.
//!
//! Nothing here can hold user data. A site is an index into a fixed set of
//! names decided at compile time, and the only values written are a count and
//! two durations. There is no reading, no surface, no session and no host
//! identity anywhere in the table, so the snapshot can be collected from a
//! real session and pasted into an issue without disclosing what was typed.
//!
//! The totals are cumulative for the life of the process and are never reset:
//! two snapshots taken around a stress run subtract cleanly, whereas a
//! resettable counter would let one reader's clear silently truncate another
//! reader's window.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use sakura_proto::{EngineTimingEntry, EngineTimingSite};

/// One site's running total.
///
/// `samples` is stored separately from `total_us` so a site that has been
/// reached but always measured zero microseconds is still distinguishable from
/// one that was never reached at all.
struct Accumulator {
    samples: AtomicU64,
    total_us: AtomicU64,
    max_us: AtomicU64,
}

impl Accumulator {
    const fn new() -> Self {
        Self {
            samples: AtomicU64::new(0),
            total_us: AtomicU64::new(0),
            max_us: AtomicU64::new(0),
        }
    }

    fn observe(&self, elapsed: Duration) {
        // Saturating rather than wrapping: an implausibly large measurement
        // has to keep reading as large. A wrapped total would report a stalled
        // engine as a fast one, which is the single most misleading value this
        // table could produce.
        let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        self.samples.fetch_add(1, Ordering::Relaxed);
        // `fetch_update` rather than `fetch_add` so the total sticks at
        // `u64::MAX` instead of rolling over after it saturates.
        let _ = self
            .total_us
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |total| {
                Some(total.saturating_add(micros))
            });
        let _ = self
            .max_us
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |max| {
                (micros > max).then_some(micros)
            });
    }

    fn read(&self, site: EngineTimingSite) -> EngineTimingEntry {
        // The three loads are not atomic with respect to each other, so a
        // snapshot taken while a request is being observed can show a sample
        // whose duration has not landed yet. That skews a mean by one
        // observation out of however many the process has served, which is
        // well inside the resolution anybody reads this table at, and is worth
        // far less than making the write path take a lock.
        EngineTimingEntry {
            site,
            samples: self.samples.load(Ordering::Relaxed),
            total_us: self.total_us.load(Ordering::Relaxed),
            max_us: self.max_us.load(Ordering::Relaxed),
        }
    }
}

/// One accumulator per site, indexed by the site's discriminant.
///
/// `EngineTimingSite::ALL` is in declaration order and the discriminants are
/// assigned explicitly from zero, so the ordinal is the index. The test at the
/// bottom of this file holds that true.
static SITES: [Accumulator; EngineTimingSite::ALL.len()] = [
    Accumulator::new(),
    Accumulator::new(),
    Accumulator::new(),
    Accumulator::new(),
    Accumulator::new(),
    Accumulator::new(),
    Accumulator::new(),
    Accumulator::new(),
    Accumulator::new(),
];

/// Records one measured span.
pub fn observe(site: EngineTimingSite, elapsed: Duration) {
    SITES[site as usize].observe(elapsed);
}

/// Times a span that ends when the guard is dropped.
///
/// A guard rather than a closure because the spans that matter most sit on a
/// path with many exits — a write failure, a disconnected client, a rejected
/// request — and a span that only recorded the path that ran to completion
/// would systematically leave out the slow ones.
#[derive(Debug)]
pub struct Span {
    site: EngineTimingSite,
    started: Instant,
}

impl Span {
    pub fn start(site: EngineTimingSite) -> Self {
        Self {
            site,
            started: Instant::now(),
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        observe(self.site, self.started.elapsed());
    }
}

/// Reads every site, in `EngineTimingSite::ALL` order.
pub fn snapshot() -> Vec<EngineTimingEntry> {
    EngineTimingSite::ALL
        .iter()
        .map(|site| SITES[*site as usize].read(*site))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table is indexed by discriminant, so a site added out of order, or
    /// with a hand-written discriminant that skips a value, would silently
    /// report another site's numbers.
    #[test]
    fn every_site_indexes_its_own_slot() {
        for (index, site) in EngineTimingSite::ALL.iter().enumerate() {
            assert_eq!(*site as usize, index, "{}", site.name());
        }
        assert_eq!(SITES.len(), EngineTimingSite::ALL.len());
    }

    #[test]
    fn a_snapshot_names_every_site_once() {
        let snapshot = snapshot();
        assert_eq!(snapshot.len(), EngineTimingSite::ALL.len());
        for (entry, site) in snapshot.iter().zip(EngineTimingSite::ALL) {
            assert_eq!(entry.site, site);
        }
    }

    /// An unreached site must not read as an instantaneous one. This is the
    /// difference between "that stage never ran in this session" and "that
    /// stage is not where the time went", and only the second one is a finding.
    #[test]
    fn a_site_with_no_samples_has_no_mean() {
        let entry = EngineTimingEntry {
            site: EngineTimingSite::Dispatch,
            samples: 0,
            total_us: 0,
            max_us: 0,
        };
        assert_eq!(entry.mean_us(), None);
    }

    #[test]
    fn totals_and_maxima_saturate_instead_of_wrapping() {
        let accumulator = Accumulator::new();
        accumulator.observe(Duration::from_micros(u64::MAX));
        accumulator.observe(Duration::from_micros(u64::MAX));
        let entry = accumulator.read(EngineTimingSite::RequestTotal);
        assert_eq!(entry.samples, 2);
        assert_eq!(entry.total_us, u64::MAX);
        assert_eq!(entry.max_us, u64::MAX);
    }

    #[test]
    fn the_maximum_keeps_the_longest_span_not_the_last() {
        let accumulator = Accumulator::new();
        accumulator.observe(Duration::from_micros(2_000));
        accumulator.observe(Duration::from_micros(5));
        let entry = accumulator.read(EngineTimingSite::Dispatch);
        assert_eq!(entry.samples, 2);
        assert_eq!(entry.max_us, 2_000);
        assert_eq!(entry.mean_us(), Some(1_002));
    }

    /// A span must record on every exit, including the ones that leave early.
    /// Those are the paths a stalled engine actually takes.
    #[test]
    fn a_span_records_when_it_is_dropped() {
        let before = snapshot()[EngineTimingSite::Encoding as usize].samples;
        {
            let _span = Span::start(EngineTimingSite::Encoding);
        }
        // Another test may be observing the same process-wide site in
        // parallel, so the assertion is that this one advanced it, not that it
        // advanced by exactly one.
        assert!(snapshot()[EngineTimingSite::Encoding as usize].samples > before);
    }
}
