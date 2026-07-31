//! Which vector instructions this machine has, decided once at startup
//! (DESIGN 3.2).
//!
//! The whole workspace is compiled with `-C target-feature=+avx`, so AVX is a
//! floor rather than something to branch on. What varies between machines is
//! everything *above* the floor: AVX2 doubles the vector width to 32 bytes,
//! AVX-512BW doubles it again to 64. Both are reached through a function
//! pointer resolved from the value this module computes, so the cost of
//! supporting three widths is one already-resolved indirect call rather than
//! a feature test inside every loop.
//!
//! # Why startup and not first use
//!
//! `is_x86_feature_detected!` caches its answer, so asking repeatedly is not
//! expensive — but it is not free either, and the place it would be asked is
//! the width normalizer, which every string leaving the engine passes
//! through. Resolving once at startup moves that work to a moment when
//! nothing is waiting on it, lets the startup log name the tier (which turns
//! "why is it slower on my machine?" into an answerable question), and turns
//! an unsupported CPU into an immediate, legible refusal instead of a fault
//! on whichever keystroke first reaches vector code.
//!
//! # The honest limit of the AVX check
//!
//! Because the binary is *built* with AVX, a machine without it can fault
//! before `main` runs, and no check written in this crate can catch that.
//! The installer's `IsProcessorFeaturePresent` gate (DESIGN 12.2) is what
//! actually protects users. [`startup`] catches the remaining case — files
//! copied onto a machine that never ran the installer — and is deliberately
//! not the primary defence.

use core::fmt;
use std::sync::OnceLock;

/// The widest kernel family this machine can run.
///
/// Ordered: a tier compares greater than every tier it subsumes, so
/// `tier() >= Tier::Avx2` is a meaningful question.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// No vector kernels — the scalar reference implementation.
    ///
    /// Reachable two ways: on a non-x86-64 target (where this crate still
    /// has to compile, since nothing in it is Windows- or x86-specific), and
    /// on an x86-64 machine below the AVX floor, where it is the safe answer
    /// to a question that should have stopped the process already.
    Scalar,
    /// The x86-64 floor: 16 bytes at a time.
    Avx,
    /// 32 bytes at a time.
    Avx2,
    /// 64 bytes at a time (AVX-512F + AVX-512BW).
    Avx512,
}

impl Tier {
    /// The name used in logs and diagnostics. Deliberately the same spelling
    /// as the `target_feature` string, so a report naming a tier can be
    /// matched against a build flag without a translation step.
    pub const fn name(self) -> &'static str {
        match self {
            Tier::Scalar => "scalar",
            Tier::Avx => "avx",
            Tier::Avx2 => "avx2",
            Tier::Avx512 => "avx512bw",
        }
    }

    /// How many bytes this tier's kernel processes per iteration. Useful for
    /// benchmarks and for tests that want to straddle a block boundary.
    pub const fn block_bytes(self) -> usize {
        match self {
            Tier::Scalar => 1,
            Tier::Avx => 16,
            Tier::Avx2 => 32,
            Tier::Avx512 => 64,
        }
    }
}

/// This CPU is below the baseline the binary was compiled for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedCpu;

impl fmt::Display for UnsupportedCpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "this CPU does not support AVX, which Sakura Input requires \
             (Intel Sandy Bridge / AMD Bulldozer, 2011 or later)",
        )
    }
}

impl std::error::Error for UnsupportedCpu {}

/// The resolved tier, computed on first call and cached for the process.
static TIER: OnceLock<Tier> = OnceLock::new();

/// Resolves the ISA tier and reports whether this machine meets the
/// baseline. Call once, early in `main`, before anything else does work.
///
/// The returned tier is what every later [`tier`] call reports, so a caller
/// that logs this value is logging the one that is actually in use.
pub fn startup() -> Result<Tier, UnsupportedCpu> {
    match detect() {
        Some(_) => Ok(tier()),
        None => Err(UnsupportedCpu),
    }
}

/// The tier resolved at startup.
///
/// Never fails, because the callers are dispatch sites with nothing sensible
/// to do about a failure: below the baseline this reports [`Tier::Scalar`],
/// which is correct — merely slow — and the process should already have
/// refused to start in [`startup`].
#[inline]
pub fn tier() -> Tier {
    *TIER.get_or_init(|| detect().unwrap_or(Tier::Scalar))
}

/// The actual `CPUID` probe. `None` means the AVX baseline is absent.
#[cfg(target_arch = "x86_64")]
fn detect() -> Option<Tier> {
    if !is_x86_feature_detected!("avx") {
        return None;
    }
    // AVX-512BW is asked for by name rather than inferred from AVX-512F:
    // the byte and word instructions live in BW, and the shuffle and mask
    // operations the kernels are built from are exactly those. A CPU with F
    // but not BW (Knights Landing) would pass an F-only check and then fail
    // to run a single instruction of the kernel.
    if is_x86_feature_detected!("avx512f") && is_x86_feature_detected!("avx512bw") {
        return Some(Tier::Avx512);
    }
    if is_x86_feature_detected!("avx2") {
        return Some(Tier::Avx2);
    }
    Some(Tier::Avx)
}

/// Off x86-64 there is no baseline to miss and no kernel to select. This
/// exists so the crate keeps compiling — and keeps passing its tests — on a
/// developer's non-Windows machine, which is worth more than the handful of
/// lines it costs.
#[cfg(not(target_arch = "x86_64"))]
fn detect() -> Option<Tier> {
    Some(Tier::Scalar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tier_is_resolved_once_and_stays_resolved() {
        let first = tier();
        assert_eq!(first, tier(), "the cached tier must not vary between calls");
    }

    /// The test binary is built with the same `+avx` flag as the shipped one,
    /// so a machine that could not run the kernels could not have run this
    /// test either.
    #[cfg(target_arch = "x86_64")]
    #[test]
    fn a_machine_that_can_run_this_test_meets_the_baseline() {
        let tier = startup().expect("this test is itself compiled with +avx");
        assert!(tier >= Tier::Avx, "resolved {tier:?}, which is below AVX");
    }

    /// The ordering is what dispatch sites ask questions with, so it has to
    /// mean "subsumes", not "was declared later".
    #[test]
    fn tiers_are_ordered_by_capability() {
        assert!(Tier::Scalar < Tier::Avx);
        assert!(Tier::Avx < Tier::Avx2);
        assert!(Tier::Avx2 < Tier::Avx512);
    }

    #[test]
    fn every_tier_has_a_name_and_a_block_size() {
        for tier in [Tier::Scalar, Tier::Avx, Tier::Avx2, Tier::Avx512] {
            assert!(!tier.name().is_empty());
            assert!(tier.block_bytes().is_power_of_two());
        }
    }

    /// The message is what a user sees when the installer's gate was
    /// bypassed, so it has to name the requirement rather than the symptom.
    #[test]
    fn the_unsupported_message_names_avx() {
        let text = UnsupportedCpu.to_string();
        assert!(text.contains("AVX"), "unhelpful message: {text}");
    }
}
