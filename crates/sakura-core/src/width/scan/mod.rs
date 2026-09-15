//! Finding the text the width policy does not have to touch, quickly.
//!
//! Most of what passes through the width choke point ([`crate::width`]) is
//! left exactly as it arrived: a half-width policy over ASCII changes
//! nothing, and kana and kanji are outside the policy's reach entirely. The
//! per-character loop still pays a classify-and-map step for every one of
//! those characters. This module finds the leading run of bytes that will
//! come out unchanged, so the normalizer can copy the run in one move and
//! spend per-character work only on the characters that actually change.
//!
//! # The trick
//!
//! For a *single-byte* character, whether it passes through unchanged is
//! decided entirely by three booleans — whether letters, digits, and symbols
//! are being widened — because that is all the policy has to say about ASCII.
//! Three booleans is eight possible answers, so all eight passthrough sets
//! are compile-time constants, and picking one is an array index rather than
//! a table build.
//!
//! Each set is stored as a nibble table small enough to live in a vector
//! register, which is what makes it testable against `vpshufb`: `LUTS[i][lo]`
//! has bit `hi` set exactly when the byte `hi << 4 | lo` passes through. A
//! vector of bytes is then classified with two shuffles and an `and`:
//!
//! 1. `lo_mask = shuffle(table, v & 0x0f)` — for each byte, the bitmask of
//!    high nibbles that would pass through with this low nibble.
//! 2. `hi_bit = shuffle(BIT_LUT, v >> 4)` — for each byte, the single bit
//!    naming its own high nibble.
//! 3. `hit = lo_mask & hi_bit` — non-zero exactly for passthrough bytes.
//!
//! Non-ASCII needs no separate check, which is the part worth pointing at:
//! `lut::BIT_LUT` is zero for high nibbles 8 through 15, so any byte with the
//! top bit set produces `hit == 0` and ends the run. A run therefore never
//! straddles a multi-byte character, and the caller can treat its end as a
//! `char` boundary without proving anything further.
//!
//! # Where this is and is not worth it
//!
//! One keystroke is one to three bytes and never reaches a vector register:
//! [`passthrough_len`] answers short inputs with the scalar scan, so the hot
//! per-key path does not even load the dispatch pointer. The wins are on the
//! strings that are actually long — committed text, candidate surfaces,
//! reconversion input — and, from M1, on dictionary search. Claiming a
//! keystroke got faster here would be dishonest.
//!
//! # Correctness
//!
//! [`scan_scalar`] is the definition of the answer; every vector kernel is
//! differential-tested against it, and the tables are tested against the
//! semantic predicate they are built from. A kernel that disagrees with the
//! reference corrupts the user's text, so agreement is asserted rather than
//! assumed (DESIGN 3.2).

#[cfg(target_arch = "x86_64")]
mod kernels_x86;
mod lut;
mod select;
#[cfg(test)]
mod testing;

#[cfg(test)]
use crate::cpu;
use crate::cpu::CpuFeatures;

pub(crate) use lut::MIN_VECTOR_BYTES;
pub use lut::{admits, passthrough_lut};
pub use select::startup;

use lut::scan_scalar;
use select::active_width_scan;
#[cfg(test)]
use testing::note_selected_width_scan_call;

// `simd_tests.rs` reaches every leaf through `use super::*;`, so the private
// items it exercises are gathered here rather than widened beyond `scan`.
#[cfg(all(test, target_arch = "x86_64"))]
use kernels_x86::{
    scan_avx2, scan_avx512_bw_vl_from_128, scan_avx512_bw_vl_from_256, scan_avx512_bw_vl_from_64,
    scan_avx_ssse3_128,
};
#[cfg(test)]
use lut::passes_through;
#[cfg(test)]
use select::resolve_kernel_set;
#[cfg(test)]
use testing::{
    avx512_path_counts_for_normalizer_runs, avx512_path_counts_for_scan,
    avx512_zmm_threshold_for_diagnostic, format_avx512_path, Avx512PathCounts, AVX_SSSE3_XMM_CALLS,
    SELECTED_WIDTH_SCAN_CALLS,
};

/// A passthrough set, indexed by low nibble: `lut[lo]` has bit `hi` set when
/// the byte `hi << 4 | lo` is left unchanged by the policy the table was
/// built for.
pub type Lut = [u8; 16];

/// A target-feature function that scans one ASCII pass-through run.
///
/// It is private because a raw function pointer would let a caller invoke an
/// instruction set that startup did not establish.  [`WidthScanStrategy`] is
/// the only safe source of one.
type ScanStrategy = unsafe fn(&[u8], &Lut) -> usize;

/// Identifier for a resolved width-scan implementation.
///
/// This describes a concrete kernel, not an ordered CPU capability tier.
/// Future AVX-512 strategies can be added without pretending that every
/// AVX-512 feature subset is "above" every other one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidthScanStrategyId {
    Scalar,
    AvxSsse3Xmm,
    Avx2Hybrid,
    /// AVX-512BW+VL, with its 64-byte ZMM body enabled from 64 bytes.
    Avx512BwVlFrom64,
    /// AVX-512BW+VL, with an exact AVX2 fallback below a 128-byte ZMM threshold.
    Avx512BwVlFrom128,
    /// AVX-512BW+VL, with an exact AVX2 fallback below a 256-byte ZMM threshold.
    Avx512BwVlFrom256,
}

/// Stable facts about one concrete kernel, for diagnostics and benchmarks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KernelMetadata {
    /// Stable implementation identifier used in startup diagnostics.
    pub id: WidthScanStrategyId,
    /// Human-readable implementation name.
    pub name: &'static str,
    /// Identifier printed by direct-kernel benchmarks.
    pub benchmark_id: &'static str,
    /// Width of the kernel's main vector loop, not a property of the CPU.
    pub block_bytes: usize,
    /// The shortest input that can use this strategy.
    pub minimum_bytes: usize,
    /// Features required to invoke this exact function safely.
    pub required_features: CpuFeatures,
}

/// The one selected implementation for ASCII pass-through-run scanning.
#[derive(Debug, Clone, Copy)]
pub struct WidthScanStrategy {
    call: ScanStrategy,
    metadata: &'static KernelMetadata,
}

impl WidthScanStrategy {
    /// Facts about this selected implementation.  Metadata is safe to expose;
    /// the function pointer remains private so callers cannot bypass startup
    /// feature validation.
    pub const fn metadata(self) -> &'static KernelMetadata {
        self.metadata
    }

    #[inline]
    unsafe fn scan(self, src: &[u8], lut: &Lut) -> usize {
        #[cfg(test)]
        note_selected_width_scan_call();
        // SAFETY: `WidthScanStrategy` values below are published only by the
        // startup resolver after checking `metadata.required_features`.
        unsafe { (self.call)(src, lut) }
    }
}

/// Kernel choices resolved once during process startup.
///
/// More independent operations can be added only when they have a measured
/// need.  Keeping this a set of concrete kernels avoids turning `CpuFeatures`
/// into a hot-path dispatch API.
#[derive(Debug, Clone, Copy)]
pub struct KernelSet {
    width_scan: WidthScanStrategy,
}

impl KernelSet {
    pub const fn width_scan(self) -> WidthScanStrategy {
        self.width_scan
    }
}

/// The minimum length at which an AVX-512BW+VL strategy starts its ZMM body.
///
/// Every one of these strategies still accepts 16-byte input: below this
/// threshold it delegates to the standard AVX2 scanner. The distinction is
/// deliberately a strategy decision, rather than a CPU-feature branch on the
/// hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Avx512ZmmThreshold {
    From64,
    From128,
    From256,
}

impl Avx512ZmmThreshold {
    const fn bytes(self) -> usize {
        match self {
            Self::From64 => 64,
            Self::From128 => 128,
            Self::From256 => 256,
        }
    }
}

/// How many leading bytes of `src` the width policy behind `lut` would leave
/// exactly as they are.
///
/// The returned length always lands on a `char` boundary, because every byte
/// it counts is ASCII (see the module docs).
pub fn passthrough_len(src: &[u8], lut: &Lut) -> usize {
    if src.len() < MIN_VECTOR_BYTES {
        return scan_scalar(src, lut);
    }
    // SAFETY: startup publishes a function pointer only after its complete
    // target-feature requirement was detected.  Before startup the immutable
    // scalar strategy is selected, which has no ISA precondition.
    unsafe { active_width_scan().scan(src, lut) }
}

#[cfg(test)]
#[path = "simd_tests.rs"]
mod tests;
