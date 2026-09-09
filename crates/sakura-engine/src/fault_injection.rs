//! Deterministic delays inserted at named points of the engine's key path.
//!
//! #148 has to answer a question that ordinary tests cannot reach: what
//! happens to a keystroke when the engine is slow in a specific way, at a
//! specific moment, for a specific length of time. The existing `BEFORE_OUTPUT`
//! hook in `server.rs` is a `#[cfg(test)]` closure and so lives only inside a
//! test binary's own process. The stress harness drives the real
//! `sakura_engine.exe` as a child, and that executable is built without
//! `cfg(test)`, so nothing in it could be delayed on purpose. This module is
//! the missing piece: an injection point that survives into the shipping
//! build, and therefore into the binary a harness can actually launch.
//!
//! # Why it is safe to ship
//!
//! Nothing here can be armed by a running system. Arming requires
//! `--fault-injection` on the command line, and `main` accepts that argument
//! only alongside a valid `--test-pipe`, whose value must live in the private
//! `\\.\pipe\SakuraInputEngineTest-` namespace. An engine serving a real user
//! binds the production name and would have refused to start at all if the
//! argument were present. There is no environment variable and no file: like
//! `--test-pipe` itself, the only way in is an explicit argument, so pipe
//! selection and fault arming stay free of ambient state.
//!
//! Arming is also one-shot. [`install`] refuses a second call, so no request
//! and no thread can arm, re-arm or disarm a point after startup. The reply to
//! `Request::FaultStatus` is the read side of that promise, and a shipping
//! engine answers it with every point disarmed — which is what makes "no delay
//! was injected" a claim someone can check rather than one they must believe.
//!
//! # Why the accounting is part of it
//!
//! A stress run that configures a two-second stall, observes no lost input and
//! never verifies the stall happened has demonstrated nothing at all. Each
//! point therefore counts how often it fired and how long it actually slept,
//! and [`status`] reports both next to the configuration. The harness asserts
//! on those numbers, not on the arguments it passed.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use sakura_proto::{FaultInjectionEntry, FaultPoint};

/// The longest single delay that may be armed.
///
/// The instruction's stress matrix tops out at a two-second engine stall, so
/// this leaves headroom without allowing a delay long enough to be mistaken
/// for a hang. An unbounded value would also make a mistyped argument
/// indistinguishable from a deadlock, which is exactly the confusion this work
/// exists to remove.
pub const MAX_DELAY: Duration = Duration::from_secs(10);

/// The longest `--fault-injection` value accepted, in bytes. Four points with
/// generous numbers fit easily; anything longer is a mistake, not a plan.
pub const MAX_SPEC_BYTES: usize = 256;

/// A parsed, validated injection plan. Holding one is not the same as arming
/// it: only [`install`] does that, and only once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    entries: Vec<PlanEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlanEntry {
    point: FaultPoint,
    delay: Duration,
    /// `u64::MAX` means "every time".
    occurrences: u64,
}

impl Plan {
    /// Parses `point=milliseconds[/occurrences]`, comma separated.
    ///
    /// Examples: `before-dispatch=50`, `after-mutation=2000/1`,
    /// `during-conversion=100,during-reply=51/3`.
    ///
    /// Every error is fatal to startup by design. A silently ignored typo
    /// would arm nothing while the harness believed it had armed something,
    /// and the run would then report a clean result that no fault ever
    /// challenged. Durations are plain milliseconds with no unit suffix so
    /// there is nothing to misread.
    pub fn parse(spec: &str) -> Result<Plan, String> {
        if spec.len() > MAX_SPEC_BYTES {
            return Err(format!(
                "--fault-injection must be at most {MAX_SPEC_BYTES} bytes"
            ));
        }
        let mut entries: Vec<PlanEntry> = Vec::new();
        for token in spec.split(',') {
            let token = token.trim();
            if token.is_empty() {
                return Err("--fault-injection has an empty entry".to_owned());
            }
            let (name, rest) = token
                .split_once('=')
                .ok_or_else(|| format!("--fault-injection entry {token:?} is not point=ms"))?;
            let point = FaultPoint::parse(name.trim()).ok_or_else(|| {
                format!(
                    "--fault-injection point {:?} is unknown; expected one of {}",
                    name.trim(),
                    FaultPoint::ALL
                        .iter()
                        .map(|p| p.name())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
            let (millis, occurrences) = match rest.split_once('/') {
                Some((millis, occurrences)) => {
                    let parsed: u64 = occurrences.trim().parse().map_err(|_| {
                        format!(
                            "--fault-injection occurrence count {occurrences:?} is not a number"
                        )
                    })?;
                    if parsed == 0 {
                        return Err(
                            "--fault-injection occurrence count must be at least 1".to_owned()
                        );
                    }
                    (millis, parsed)
                }
                None => (rest, u64::MAX),
            };
            let millis: u64 = millis
                .trim()
                .parse()
                .map_err(|_| format!("--fault-injection delay {millis:?} is not a number of ms"))?;
            let delay = Duration::from_millis(millis);
            if delay > MAX_DELAY {
                return Err(format!(
                    "--fault-injection delay {millis} ms exceeds the {} ms maximum",
                    MAX_DELAY.as_millis()
                ));
            }
            if delay.is_zero() {
                return Err(
                    "--fault-injection delay must be at least 1 ms; omit the point to disarm it"
                        .to_owned(),
                );
            }
            if entries.iter().any(|existing| existing.point == point) {
                return Err(format!("--fault-injection names {} twice", point.name()));
            }
            entries.push(PlanEntry {
                point,
                delay,
                occurrences,
            });
        }
        if entries.is_empty() {
            return Err("--fault-injection requires at least one entry".to_owned());
        }
        Ok(Plan { entries })
    }

    /// A one-line, content-free description for the startup log.
    pub fn describe(&self) -> String {
        self.entries
            .iter()
            .map(|entry| {
                let occurrences = if entry.occurrences == u64::MAX {
                    "every".to_owned()
                } else {
                    entry.occurrences.to_string()
                };
                format!(
                    "{}={}ms/{occurrences}",
                    entry.point.name(),
                    entry.delay.as_millis()
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    }
}

struct Slot {
    delay_us: AtomicU64,
    remaining: AtomicU64,
    fired: AtomicU64,
    slept_us: AtomicU64,
}

impl Slot {
    const fn new() -> Self {
        Slot {
            delay_us: AtomicU64::new(0),
            remaining: AtomicU64::new(0),
            fired: AtomicU64::new(0),
            slept_us: AtomicU64::new(0),
        }
    }

    /// Claims one occurrence, returning the delay to sleep for.
    ///
    /// The claim is a compare-exchange loop rather than a plain decrement so
    /// that a point armed for exactly one occurrence delays exactly one
    /// request even when several connections reach it at the same instant.
    /// "Exactly once" has to hold for the instrument too, or the harness
    /// cannot attribute what it sees to the fault it asked for.
    fn claim(&self) -> Option<Duration> {
        let delay_us = self.delay_us.load(Ordering::Relaxed);
        if delay_us == 0 {
            return None;
        }
        let mut remaining = self.remaining.load(Ordering::Relaxed);
        loop {
            if remaining == 0 {
                return None;
            }
            if remaining == u64::MAX {
                break;
            }
            match self.remaining.compare_exchange_weak(
                remaining,
                remaining - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(observed) => remaining = observed,
            }
        }
        Some(Duration::from_micros(delay_us))
    }

    fn read(&self, point: FaultPoint) -> FaultInjectionEntry {
        FaultInjectionEntry {
            point,
            delay_us: self.delay_us.load(Ordering::Relaxed),
            remaining: self.remaining.load(Ordering::Relaxed),
            fired: self.fired.load(Ordering::Relaxed),
            slept_us: self.slept_us.load(Ordering::Relaxed),
        }
    }
}

static SLOTS: [Slot; FaultPoint::ALL.len()] = [Slot::new(), Slot::new(), Slot::new(), Slot::new()];

/// The single check on the ordinary path. While this is false — which is
/// always, in a shipping engine — [`delay`] is one relaxed load and a return.
static ARMED: AtomicBool = AtomicBool::new(false);
static INSTALLED: AtomicBool = AtomicBool::new(false);

/// Arms a plan. Refuses a second call.
///
/// The caller is responsible for the safety gate; see the module docs. Making
/// this callable only once means nothing that happens after startup — no
/// request, no thread, no reconnection — can change what is armed, so the
/// status reply describes the whole life of the process.
pub fn install(plan: &Plan) -> Result<(), String> {
    if INSTALLED.swap(true, Ordering::SeqCst) {
        return Err("fault injection has already been installed".to_owned());
    }
    for entry in &plan.entries {
        let slot = &SLOTS[entry.point as usize];
        let micros = u64::try_from(entry.delay.as_micros()).unwrap_or(u64::MAX);
        slot.delay_us.store(micros, Ordering::Relaxed);
        slot.remaining.store(entry.occurrences, Ordering::Relaxed);
    }
    ARMED.store(true, Ordering::SeqCst);
    Ok(())
}

/// Sleeps if this point is armed and has occurrences left. Otherwise returns
/// immediately.
pub fn delay(point: FaultPoint) {
    if !ARMED.load(Ordering::Relaxed) {
        return;
    }
    let slot = &SLOTS[point as usize];
    let Some(requested) = slot.claim() else {
        return;
    };
    let started = Instant::now();
    std::thread::sleep(requested);
    // The measured sleep, not the requested one. A loaded scheduler can
    // oversleep by a wide margin, and reporting the request would understate
    // the stall the run actually contained.
    let actual = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    slot.fired.fetch_add(1, Ordering::Relaxed);
    let _ = slot
        .slept_us
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |total| {
            Some(total.saturating_add(actual))
        });
}

/// One entry per [`FaultPoint`], in declaration order.
pub fn status() -> Vec<FaultInjectionEntry> {
    FaultPoint::ALL
        .iter()
        .map(|point| SLOTS[*point as usize].read(*point))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_point_indexes_its_own_slot() {
        // A shifted index would attribute one point's delay to another, and
        // the resulting report would name the wrong moment in the key path.
        for (index, point) in FaultPoint::ALL.iter().enumerate() {
            assert_eq!(*point as usize, index, "{} is misplaced", point.name());
        }
        assert_eq!(SLOTS.len(), FaultPoint::ALL.len());
    }

    #[test]
    fn a_status_names_every_point_once_and_is_disarmed_by_default() {
        let entries = status();
        assert_eq!(entries.len(), FaultPoint::ALL.len());
        for (entry, point) in entries.iter().zip(FaultPoint::ALL) {
            assert_eq!(entry.point, point);
            // This test binary never installs a plan, which is the same
            // situation a shipping engine is always in.
            assert!(!entry.is_armed(), "{} is armed", point.name());
        }
    }

    #[test]
    fn a_plan_parses_points_delays_and_occurrence_counts() {
        let plan = Plan::parse("before-dispatch=50,after-mutation=2000/1").expect("parse");
        assert_eq!(
            plan.entries,
            vec![
                PlanEntry {
                    point: FaultPoint::BeforeDispatch,
                    delay: Duration::from_millis(50),
                    occurrences: u64::MAX,
                },
                PlanEntry {
                    point: FaultPoint::AfterMutation,
                    delay: Duration::from_millis(2000),
                    occurrences: 1,
                },
            ]
        );
        assert_eq!(
            plan.describe(),
            "before-dispatch=50ms/every,after-mutation=2000ms/1"
        );
    }

    /// Every one of these would otherwise arm nothing while the caller
    /// believed it had armed something, and the run would report a clean
    /// result that no fault ever challenged.
    #[test]
    fn a_malformed_plan_is_refused_rather_than_partly_applied() {
        for spec in [
            "",
            "before-dispatch",
            "before-dispatch=",
            "before-dispatch=abc",
            "before-dispach=50",
            "during-render=50",
            "before-dispatch=0",
            "before-dispatch=10001",
            "before-dispatch=50/0",
            "before-dispatch=50/x",
            "before-dispatch=50,before-dispatch=60",
            "before-dispatch=50,",
        ] {
            assert!(Plan::parse(spec).is_err(), "{spec:?} was accepted");
        }
        assert!(Plan::parse(&"a".repeat(MAX_SPEC_BYTES + 1)).is_err());
    }

    #[test]
    fn the_maximum_delay_is_the_boundary_not_an_open_range() {
        let at_maximum = format!("before-dispatch={}", MAX_DELAY.as_millis());
        assert!(Plan::parse(&at_maximum).is_ok());
        let past_maximum = format!("before-dispatch={}", MAX_DELAY.as_millis() + 1);
        assert!(Plan::parse(&past_maximum).is_err());
    }

    /// A bounded plan must spend exactly its budget. If a slot could be
    /// claimed more often than it was armed for, a "one stall" test would
    /// silently become a "stall every key" test.
    #[test]
    fn a_bounded_slot_yields_exactly_its_configured_number_of_claims() {
        let slot = Slot::new();
        slot.delay_us.store(1_000, Ordering::Relaxed);
        slot.remaining.store(2, Ordering::Relaxed);
        assert_eq!(slot.claim(), Some(Duration::from_millis(1)));
        assert_eq!(slot.claim(), Some(Duration::from_millis(1)));
        assert_eq!(slot.claim(), None);
        assert_eq!(slot.remaining.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn an_unbounded_slot_never_exhausts_and_a_disarmed_one_never_claims() {
        let unbounded = Slot::new();
        unbounded.delay_us.store(5, Ordering::Relaxed);
        unbounded.remaining.store(u64::MAX, Ordering::Relaxed);
        for _ in 0..1_000 {
            assert!(unbounded.claim().is_some());
        }
        assert_eq!(unbounded.remaining.load(Ordering::Relaxed), u64::MAX);

        // Disarmed by delay, even though occurrences remain.
        let no_delay = Slot::new();
        no_delay.remaining.store(u64::MAX, Ordering::Relaxed);
        assert_eq!(no_delay.claim(), None);
    }

    #[test]
    fn a_disarmed_point_reports_no_activity_and_returns_immediately() {
        let before = status();
        let started = Instant::now();
        delay(FaultPoint::BeforeDispatch);
        assert!(started.elapsed() < Duration::from_millis(50));
        assert_eq!(before, status());
    }
}
