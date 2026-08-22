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
//! [`BIT_LUT`] is zero for high nibbles 8 through 15, so any byte with the
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

use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};
use std::sync::OnceLock;

use crate::cpu::{self, CpuFeatures, UnsupportedCpu};

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

/// `1 << hi` for each high nibble an ASCII byte can have, and zero for the
/// eight that only a continuation or lead byte can have. Those zeros are
/// what make non-ASCII end a run without a second comparison.
const BIT_LUT: Lut = [1, 2, 4, 8, 16, 32, 64, 128, 0, 0, 0, 0, 0, 0, 0, 0];

/// Below this many bytes the scalar scan wins outright, and skipping the
/// dispatch keeps the per-keystroke path as short as it was before this
/// module existed.
///
/// Visible to the crate because it is also the line below which a *caller*
/// should not bother scanning at all: one narrow vector block is the least
/// work that can repay resolving a table and entering a dispatch, so a
/// string shorter than this is cheaper handled character by character
/// (see `width::Normalizer::normalize_into`).
pub(crate) const MIN_VECTOR_BYTES: usize = 16;

/// Whether the width policy leaves the single-byte character `b` alone.
///
/// This is the semantic definition, mirroring [`crate::width::normalize_char`]
/// restricted to ASCII: letters follow the alnum channel, digits the number
/// channel, ASCII punctuation the symbol channel, space always passes
/// (SpaceWidth owns it), and control characters are outside the policy
/// entirely. Japanese punctuation cannot
/// appear here — none of the four code points it owns is ASCII.
///
/// Bytes at or above `0x80` are not single-byte characters at all; callers
/// must reject them before consulting this, which the tables do structurally
/// by leaving those high nibbles unset.
const fn passes_through(b: u8, full_alpha: bool, full_digit: bool, full_symbol: bool) -> bool {
    match b {
        b'A'..=b'Z' | b'a'..=b'z' => !full_alpha,
        b'0'..=b'9' => !full_digit,
        // ASCII space is owned by SpaceWidth, not the symbol channel, so it
        // always passes through. Remaining ASCII punctuation follows symbol.
        b' ' => true,
        0x21..=0x2F | 0x3A..=0x40 | 0x5B..=0x60 | 0x7B..=0x7E => !full_symbol,
        // 0x00..=0x1F and 0x7F: the width policy has no opinion, so they
        // always pass. So does everything from 0x80 up, but see the note
        // above — those never reach a table lookup.
        _ => true,
    }
}

/// Builds the passthrough table for one combination of the three channels.
const fn build_lut(full_alpha: bool, full_digit: bool, full_symbol: bool) -> Lut {
    let mut lut = [0u8; 16];
    let mut hi = 0usize;
    while hi < 8 {
        let mut lo = 0usize;
        while lo < 16 {
            if passes_through((hi << 4 | lo) as u8, full_alpha, full_digit, full_symbol) {
                lut[lo] |= 1 << hi;
            }
            lo += 1;
        }
        hi += 1;
    }
    lut
}

/// All eight passthrough sets, built at compile time. Indexed by
/// [`lut_index`].
const LUTS: [Lut; 8] = {
    let mut luts = [[0u8; 16]; 8];
    let mut i = 0usize;
    while i < 8 {
        luts[i] = build_lut(i & 0b100 != 0, i & 0b010 != 0, i & 0b001 != 0);
        i += 1;
    }
    luts
};

/// Packs the three channel decisions into an index into [`LUTS`].
const fn lut_index(full_alpha: bool, full_digit: bool, full_symbol: bool) -> usize {
    ((full_alpha as usize) << 2) | ((full_digit as usize) << 1) | (full_symbol as usize)
}

/// The passthrough set for a resolved policy.
///
/// The arguments are already-resolved half/full decisions, not `Width`
/// settings: resolving `FollowMode` against the current mode is the caller's
/// job and happens once per call rather than once per character.
pub fn passthrough_lut(full_alpha: bool, full_digit: bool, full_symbol: bool) -> &'static Lut {
    &LUTS[lut_index(full_alpha, full_digit, full_symbol)]
}

const SCALAR_METADATA: KernelMetadata = KernelMetadata {
    id: WidthScanStrategyId::Scalar,
    name: "scalar",
    benchmark_id: "scan-scalar",
    block_bytes: 1,
    minimum_bytes: 0,
    required_features: CpuFeatures::EMPTY,
};

static SCALAR_WIDTH_SCAN: WidthScanStrategy = WidthScanStrategy {
    call: scan_scalar as ScanStrategy,
    metadata: &SCALAR_METADATA,
};

#[cfg(target_arch = "x86_64")]
const AVX_SSSE3_XMM_METADATA: KernelMetadata = KernelMetadata {
    id: WidthScanStrategyId::AvxSsse3Xmm,
    name: "avx-ssse3-128",
    benchmark_id: "scan-avx-ssse3-128",
    block_bytes: 16,
    minimum_bytes: MIN_VECTOR_BYTES,
    required_features: CpuFeatures::AVX_SSSE3,
};

#[cfg(target_arch = "x86_64")]
static AVX_SSSE3_XMM_WIDTH_SCAN: WidthScanStrategy = WidthScanStrategy {
    call: scan_avx_ssse3_128 as ScanStrategy,
    metadata: &AVX_SSSE3_XMM_METADATA,
};

#[cfg(target_arch = "x86_64")]
const AVX2_HYBRID_METADATA: KernelMetadata = KernelMetadata {
    id: WidthScanStrategyId::Avx2Hybrid,
    name: "avx2-hybrid",
    benchmark_id: "scan-avx2-hybrid",
    block_bytes: 32,
    minimum_bytes: MIN_VECTOR_BYTES,
    required_features: CpuFeatures::AVX2,
};

#[cfg(target_arch = "x86_64")]
static AVX2_HYBRID_WIDTH_SCAN: WidthScanStrategy = WidthScanStrategy {
    call: scan_avx2 as ScanStrategy,
    metadata: &AVX2_HYBRID_METADATA,
};

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

#[cfg(target_arch = "x86_64")]
const AVX512_BW_VL_FROM_64_METADATA: KernelMetadata = KernelMetadata {
    id: WidthScanStrategyId::Avx512BwVlFrom64,
    name: "avx512bw-vl-from-64",
    benchmark_id: "scan-avx512bw-vl-from-64",
    block_bytes: 64,
    minimum_bytes: MIN_VECTOR_BYTES,
    required_features: CpuFeatures::AVX512_BW_VL,
};

#[cfg(target_arch = "x86_64")]
static AVX512_BW_VL_FROM_64_WIDTH_SCAN: WidthScanStrategy = WidthScanStrategy {
    call: scan_avx512_bw_vl_from_64 as ScanStrategy,
    metadata: &AVX512_BW_VL_FROM_64_METADATA,
};

#[cfg(target_arch = "x86_64")]
const AVX512_BW_VL_FROM_128_METADATA: KernelMetadata = KernelMetadata {
    id: WidthScanStrategyId::Avx512BwVlFrom128,
    name: "avx512bw-vl-from-128",
    benchmark_id: "scan-avx512bw-vl-from-128",
    block_bytes: 64,
    minimum_bytes: MIN_VECTOR_BYTES,
    required_features: CpuFeatures::AVX512_BW_VL,
};

#[cfg(target_arch = "x86_64")]
static AVX512_BW_VL_FROM_128_WIDTH_SCAN: WidthScanStrategy = WidthScanStrategy {
    call: scan_avx512_bw_vl_from_128 as ScanStrategy,
    metadata: &AVX512_BW_VL_FROM_128_METADATA,
};

#[cfg(target_arch = "x86_64")]
const AVX512_BW_VL_FROM_256_METADATA: KernelMetadata = KernelMetadata {
    id: WidthScanStrategyId::Avx512BwVlFrom256,
    name: "avx512bw-vl-from-256",
    benchmark_id: "scan-avx512bw-vl-from-256",
    block_bytes: 64,
    minimum_bytes: MIN_VECTOR_BYTES,
    required_features: CpuFeatures::AVX512_BW_VL,
};

#[cfg(target_arch = "x86_64")]
static AVX512_BW_VL_FROM_256_WIDTH_SCAN: WidthScanStrategy = WidthScanStrategy {
    call: scan_avx512_bw_vl_from_256 as ScanStrategy,
    metadata: &AVX512_BW_VL_FROM_256_METADATA,
};

// Production deliberately does not reference the bench-only AVX-512 records,
// so an optimizing release build is entitled to remove their bodies. The
// isolated assembly-audit artifact enables this feature to retain precisely
// those function pointers for instruction-shape validation without requiring
// the shipping build to retain them solely for the audit.
#[cfg(all(target_arch = "x86_64", feature = "simd-assembly-audit"))]
#[used]
static SIMD_ASSEMBLY_AUDIT_AVX512_STRATEGIES: [ScanStrategy; 3] = [
    scan_avx512_bw_vl_from_64 as ScanStrategy,
    scan_avx512_bw_vl_from_128 as ScanStrategy,
    scan_avx512_bw_vl_from_256 as ScanStrategy,
];

/// The process-wide width scanner.  It begins as scalar so library users and
/// tests remain correct before the engine has started; engine startup replaces
/// it with one of the immutable strategy records above before worker threads
/// are spawned.  A hot scan performs one pointer load and one indirect kernel
/// call, with no CPUID probe, feature match, or lazy initialization check.
static ACTIVE_WIDTH_SCAN: AtomicPtr<WidthScanStrategy> =
    AtomicPtr::new(ptr::addr_of!(SCALAR_WIDTH_SCAN).cast_mut());

/// Startup-only memoization.  This is intentionally separate from
/// [`ACTIVE_WIDTH_SCAN`]: it is never read by the width-normalization hot
/// path, which reads the atomic pointer above directly.
static STARTUP_KERNEL_SET: OnceLock<Result<KernelSet, UnsupportedCpu>> = OnceLock::new();

/// Detects CPU features and publishes exactly one width-scan kernel for this
/// process.
///
/// Call this before creating engine workers.  Repeated calls return the first
/// resolved set; no call after the first repeats CPUID.  The selected function
/// pointer always refers to immutable static storage, so replacing the active
/// pointer is safe even if a diagnostics caller asks for the already-resolved
/// set later.
pub fn startup() -> Result<KernelSet, UnsupportedCpu> {
    let result = *STARTUP_KERNEL_SET.get_or_init(resolve_kernel_set_at_startup);
    if let Ok(kernel_set) = result {
        ACTIVE_WIDTH_SCAN.store(strategy_pointer(kernel_set.width_scan), Ordering::Release);
    }
    result
}

fn resolve_kernel_set_at_startup() -> Result<KernelSet, UnsupportedCpu> {
    let features = cpu::detect_at_startup()?;
    // AVX-512BW+VL remains bench-only until the direct-kernel result is backed
    // by stable end-to-end and cross-host evidence. Production still resolves
    // CPU capability exactly once, but publishes AVX2 (or the AVX+SSSE3 floor)
    // rather than treating one host's timing result as a universal admission.
    resolve_kernel_set(features, None)
}

/// Resolves a concrete kernel from a raw feature set.  This stays private to
/// startup and synthetic tests; ordinary callers can only use [`KernelSet`].
/// Production passes `None`; a non-`None` AVX-512 threshold is a synthetic
/// proof that an external admission policy established the exact requirements.
#[cfg(target_arch = "x86_64")]
fn resolve_kernel_set(
    features: CpuFeatures,
    avx512_threshold: Option<Avx512ZmmThreshold>,
) -> Result<KernelSet, UnsupportedCpu> {
    if !features.supports(CpuFeatures::AVX_SSSE3) {
        return Err(UnsupportedCpu);
    }

    // AVX2 is the standard fast path. A CPU's CPUID bit alone is never enough
    // to select AVX-512; only an externally established explicit admission can
    // reach one of the dormant candidate records below.
    let width_scan = match avx512_threshold {
        Some(Avx512ZmmThreshold::From64) if features.supports(CpuFeatures::AVX512_BW_VL) => {
            AVX512_BW_VL_FROM_64_WIDTH_SCAN
        }
        Some(Avx512ZmmThreshold::From128) if features.supports(CpuFeatures::AVX512_BW_VL) => {
            AVX512_BW_VL_FROM_128_WIDTH_SCAN
        }
        Some(Avx512ZmmThreshold::From256) if features.supports(CpuFeatures::AVX512_BW_VL) => {
            AVX512_BW_VL_FROM_256_WIDTH_SCAN
        }
        _ if features.supports(CpuFeatures::AVX2) => AVX2_HYBRID_WIDTH_SCAN,
        _ => AVX_SSSE3_XMM_WIDTH_SCAN,
    };
    Ok(KernelSet { width_scan })
}

#[cfg(not(target_arch = "x86_64"))]
fn resolve_kernel_set(
    _features: CpuFeatures,
    _avx512_threshold: Option<Avx512ZmmThreshold>,
) -> Result<KernelSet, UnsupportedCpu> {
    Ok(KernelSet {
        width_scan: SCALAR_WIDTH_SCAN,
    })
}

#[cfg(target_arch = "x86_64")]
fn strategy_pointer(strategy: WidthScanStrategy) -> *mut WidthScanStrategy {
    match strategy.metadata().id {
        WidthScanStrategyId::Scalar => ptr::addr_of!(SCALAR_WIDTH_SCAN).cast_mut(),
        WidthScanStrategyId::AvxSsse3Xmm => ptr::addr_of!(AVX_SSSE3_XMM_WIDTH_SCAN).cast_mut(),
        WidthScanStrategyId::Avx2Hybrid => ptr::addr_of!(AVX2_HYBRID_WIDTH_SCAN).cast_mut(),
        WidthScanStrategyId::Avx512BwVlFrom64 => {
            ptr::addr_of!(AVX512_BW_VL_FROM_64_WIDTH_SCAN).cast_mut()
        }
        WidthScanStrategyId::Avx512BwVlFrom128 => {
            ptr::addr_of!(AVX512_BW_VL_FROM_128_WIDTH_SCAN).cast_mut()
        }
        WidthScanStrategyId::Avx512BwVlFrom256 => {
            ptr::addr_of!(AVX512_BW_VL_FROM_256_WIDTH_SCAN).cast_mut()
        }
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn strategy_pointer(strategy: WidthScanStrategy) -> *mut WidthScanStrategy {
    debug_assert_eq!(strategy.metadata().id, WidthScanStrategyId::Scalar);
    ptr::addr_of!(SCALAR_WIDTH_SCAN).cast_mut()
}

#[inline]
fn active_width_scan() -> &'static WidthScanStrategy {
    // SAFETY: initialized to `SCALAR_WIDTH_SCAN` and subsequently written only
    // with addresses of the immutable static strategy records above.
    unsafe { &*ACTIVE_WIDTH_SCAN.load(Ordering::Acquire) }
}

#[cfg(test)]
std::thread_local! {
    static SELECTED_WIDTH_SCAN_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static AVX_SSSE3_XMM_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test-only evidence of which part of the AVX-512 hybrid a given scan uses.
///
/// This is derived outside the target-feature function, rather than incremented
/// inside its loops. Consequently, a release test benchmark measures the same
/// instruction sequence and register allocation as the shipped scanner.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Avx512PathCounts {
    avx2_ymm_blocks: usize,
    avx2_xmm_blocks: usize,
    zmm_blocks: usize,
    vl_ymm_blocks: usize,
    vl_xmm_blocks: usize,
}

#[cfg(test)]
fn note_selected_width_scan_call() {
    SELECTED_WIDTH_SCAN_CALLS.with(|calls| calls.set(calls.get() + 1));
}

#[cfg(test)]
fn note_avx_ssse3_xmm_call() {
    AVX_SSSE3_XMM_CALLS.with(|calls| calls.set(calls.get() + 1));
}

#[cfg(test)]
fn avx512_path_counts_for_scan(src: &[u8], lut: &Lut, zmm_min_bytes: usize) -> Avx512PathCounts {
    const ZMM_LANES: usize = 64;
    const YMM_LANES: usize = 32;
    const XMM_LANES: usize = 16;

    let mut at = 0;
    let mut counts = Avx512PathCounts::default();
    if src.len() < zmm_min_bytes {
        while at + YMM_LANES <= src.len() {
            counts.avx2_ymm_blocks += 1;
            if scan_scalar(&src[at..at + YMM_LANES], lut) != YMM_LANES {
                return counts;
            }
            at += YMM_LANES;
        }
        if at + XMM_LANES <= src.len() {
            counts.avx2_xmm_blocks += 1;
        }
        return counts;
    }
    while at + ZMM_LANES <= src.len() {
        counts.zmm_blocks += 1;
        if scan_scalar(&src[at..at + ZMM_LANES], lut) != ZMM_LANES {
            return counts;
        }
        at += ZMM_LANES;
    }
    while at + YMM_LANES <= src.len() {
        counts.vl_ymm_blocks += 1;
        if scan_scalar(&src[at..at + YMM_LANES], lut) != YMM_LANES {
            return counts;
        }
        at += YMM_LANES;
    }
    if at + XMM_LANES <= src.len() {
        counts.vl_xmm_blocks += 1;
    }
    counts
}

#[cfg(test)]
fn format_avx512_path(counts: Avx512PathCounts) -> String {
    format!(
        "a2y={} a2x={} z={} vy={} vx={}",
        counts.avx2_ymm_blocks,
        counts.avx2_xmm_blocks,
        counts.zmm_blocks,
        counts.vl_ymm_blocks,
        counts.vl_xmm_blocks
    )
}

/// Replays the normalizer's run boundaries solely to describe one post-timing
/// diagnostic sample. This does not take part in either side of an A/B timing
/// pair; the raw kernel remains the thing being measured.
#[cfg(test)]
fn avx512_path_counts_for_normalizer_runs(
    src: &str,
    lut: &Lut,
    zmm_min_bytes: usize,
) -> Avx512PathCounts {
    let mut rest = src;
    let mut total = Avx512PathCounts::default();
    while let Some(&first) = rest.as_bytes().first() {
        if admits(lut, first) {
            if rest.len() >= MIN_VECTOR_BYTES {
                let path = avx512_path_counts_for_scan(rest.as_bytes(), lut, zmm_min_bytes);
                total.avx2_ymm_blocks += path.avx2_ymm_blocks;
                total.avx2_xmm_blocks += path.avx2_xmm_blocks;
                total.zmm_blocks += path.zmm_blocks;
                total.vl_ymm_blocks += path.vl_ymm_blocks;
                total.vl_xmm_blocks += path.vl_xmm_blocks;
            }
            // A leading pass-through run is ASCII-only, so its end is also a
            // valid UTF-8 boundary, exactly as in `Normalizer::normalize_runs`.
            let run = scan_scalar(rest.as_bytes(), lut);
            rest = &rest[run..];
        } else {
            let mut chars = rest.chars();
            let _ = chars.next().expect("non-empty `rest` has a first char");
            rest = chars.as_str();
        }
    }
    total
}

/// Whether `lut` says byte `b` passes through. The `b < 0x80` guard is load
/// bearing twice over: it keeps non-ASCII out of the table, and it keeps
/// `1 << (b >> 4)` from shifting a `u8` by more than seven.
///
/// Public because it is the cheap way to ask "does a run even start here?".
/// Japanese text stops a run at every character, and a caller that asked for
/// a scan anyway would pay a vector load per kana to be told zero.
#[inline]
pub fn admits(lut: &Lut, b: u8) -> bool {
    b < 0x80 && (lut[(b & 0x0f) as usize] & (1 << (b >> 4))) != 0
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

/// The reference implementation: the definition of the right answer, and the
/// tail of every vector kernel.
fn scan_scalar(src: &[u8], lut: &Lut) -> usize {
    src.iter()
        .position(|&b| !admits(lut, b))
        .unwrap_or(src.len())
}

/// 16 bytes at a time — the x86-64 floor (DESIGN 3.2).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx,ssse3")]
unsafe fn scan_avx_ssse3_128(src: &[u8], lut: &Lut) -> usize {
    #[cfg(test)]
    note_avx_ssse3_xmm_call();

    use core::arch::x86_64::{
        __m128i, _mm_and_si128, _mm_cmpeq_epi8, _mm_loadu_si128, _mm_movemask_epi8, _mm_set1_epi8,
        _mm_setzero_si128, _mm_shuffle_epi8, _mm_srli_epi16,
    };

    const LANES: usize = 16;
    let mut at = 0usize;
    // SAFETY: every load reads exactly `LANES` bytes starting at `at`, and
    // the loop condition proves those bytes are within `src`; the two tables
    // are 16-byte arrays read whole. All loads are the unaligned form, so no
    // alignment is assumed. The instructions require AVX and SSSE3, which
    // `#[target_feature]` makes this function's precondition.
    unsafe {
        let table = _mm_loadu_si128(lut.as_ptr().cast::<__m128i>());
        let bits = _mm_loadu_si128(BIT_LUT.as_ptr().cast::<__m128i>());
        let low = _mm_set1_epi8(0x0f);
        let zero = _mm_setzero_si128();
        while at + LANES <= src.len() {
            let v = _mm_loadu_si128(src.as_ptr().add(at).cast::<__m128i>());
            // The high nibble via a 16-bit shift: the bits that leak in from
            // the neighbouring byte all land above bit 3 and the mask
            // discards them, leaving exactly `byte >> 4` in every lane.
            let lo = _mm_and_si128(v, low);
            let hi = _mm_and_si128(_mm_srli_epi16::<4>(v), low);
            let hit = _mm_and_si128(_mm_shuffle_epi8(table, lo), _mm_shuffle_epi8(bits, hi));
            let stop = _mm_movemask_epi8(_mm_cmpeq_epi8(hit, zero)) as u32;
            if stop != 0 {
                return at + stop.trailing_zeros() as usize;
            }
            at += LANES;
        }
    }
    at + scan_scalar(&src[at..], lut)
}

/// 32 bytes at a time, then one 16-byte XMM tail in this same AVX2 body, so
/// the remaining scalar tail is never more than 15 bytes.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,ssse3")]
unsafe fn scan_avx2(src: &[u8], lut: &Lut) -> usize {
    use core::arch::x86_64::{
        __m128i, __m256i, _mm256_and_si256, _mm256_broadcastsi128_si256, _mm256_cmpeq_epi8,
        _mm256_loadu_si256, _mm256_movemask_epi8, _mm256_set1_epi8, _mm256_setzero_si256,
        _mm256_shuffle_epi8, _mm256_srli_epi16, _mm_and_si128, _mm_cmpeq_epi8, _mm_loadu_si128,
        _mm_movemask_epi8, _mm_set1_epi8, _mm_setzero_si128, _mm_shuffle_epi8, _mm_srli_epi16,
    };

    const YMM_LANES: usize = 32;
    const XMM_LANES: usize = 16;
    let mut at = 0usize;
    // SAFETY: 32-byte and 16-byte loads are guarded by their respective loop
    // conditions. `_mm256_shuffle_epi8` works within each 128-bit half, so
    // the tables are broadcast to both halves. The XMM tail uses the same
    // table representation and remains inside this AVX2 target-feature body.
    unsafe {
        let table_xmm = _mm_loadu_si128(lut.as_ptr().cast::<__m128i>());
        let bits_xmm = _mm_loadu_si128(BIT_LUT.as_ptr().cast::<__m128i>());
        let low_xmm = _mm_set1_epi8(0x0f);
        let zero_xmm = _mm_setzero_si128();

        // Do not construct YMM broadcasts for a 16--31 byte input.  The
        // selected AVX2 strategy owns that XMM-sized range too, but it should
        // not pay setup for a 32-byte loop that cannot run.
        if src.len() >= YMM_LANES {
            let table_ymm = _mm256_broadcastsi128_si256(table_xmm);
            let bits_ymm = _mm256_broadcastsi128_si256(bits_xmm);
            let low_ymm = _mm256_set1_epi8(0x0f);
            let zero_ymm = _mm256_setzero_si256();
            while at + YMM_LANES <= src.len() {
                let v = _mm256_loadu_si256(src.as_ptr().add(at).cast::<__m256i>());
                let lo = _mm256_and_si256(v, low_ymm);
                let hi = _mm256_and_si256(_mm256_srli_epi16::<4>(v), low_ymm);
                let hit = _mm256_and_si256(
                    _mm256_shuffle_epi8(table_ymm, lo),
                    _mm256_shuffle_epi8(bits_ymm, hi),
                );
                let stop = _mm256_movemask_epi8(_mm256_cmpeq_epi8(hit, zero_ymm)) as u32;
                if stop != 0 {
                    return at + stop.trailing_zeros() as usize;
                }
                at += YMM_LANES;
            }
        }

        if at + XMM_LANES <= src.len() {
            let v = _mm_loadu_si128(src.as_ptr().add(at).cast::<__m128i>());
            let lo = _mm_and_si128(v, low_xmm);
            let hi = _mm_and_si128(_mm_srli_epi16::<4>(v), low_xmm);
            let hit = _mm_and_si128(
                _mm_shuffle_epi8(table_xmm, lo),
                _mm_shuffle_epi8(bits_xmm, hi),
            );
            let stop = _mm_movemask_epi8(_mm_cmpeq_epi8(hit, zero_xmm)) as u32;
            if stop != 0 {
                return at + stop.trailing_zeros() as usize;
            }
            at += XMM_LANES;
        }

        at + scan_scalar(&src[at..], lut)
    }
}

/// AVX-512BW+VL strategy whose 64-byte body begins as soon as one full ZMM is
/// available. Below that point it delegates to the exact AVX2 scanner: a
/// 16--63 byte input is not evidence that any AVX-512 work is beneficial.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,avx512f,avx512bw,avx512vl,ssse3")]
unsafe fn scan_avx512_bw_vl_from_64(src: &[u8], lut: &Lut) -> usize {
    // SAFETY: this wrapper carries the same complete target-feature contract
    // as the shared body, and only supplies one of its fixed thresholds.
    unsafe { scan_avx512_bw_vl_hybrid(src, lut, Avx512ZmmThreshold::From64.bytes()) }
}

/// AVX-512BW+VL strategy that reserves its ZMM body for 128-byte and longer
/// inputs.  It is selected only if startup measurements reject the 64-byte
/// takeover but accept this narrower owned range.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,avx512f,avx512bw,avx512vl,ssse3")]
unsafe fn scan_avx512_bw_vl_from_128(src: &[u8], lut: &Lut) -> usize {
    // SAFETY: see `scan_avx512_bw_vl_from_64`.
    unsafe { scan_avx512_bw_vl_hybrid(src, lut, Avx512ZmmThreshold::From128.bytes()) }
}

/// AVX-512BW+VL strategy that reserves its ZMM body for 256-byte and longer
/// inputs.  This is the conservative final candidate when shorter long runs do
/// not clear the AVX2-relative gate on a particular processor.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,avx512f,avx512bw,avx512vl,ssse3")]
unsafe fn scan_avx512_bw_vl_from_256(src: &[u8], lut: &Lut) -> usize {
    // SAFETY: see `scan_avx512_bw_vl_from_64`.
    unsafe { scan_avx512_bw_vl_hybrid(src, lut, Avx512ZmmThreshold::From256.bytes()) }
}

/// Shared AVX-512BW+VL scanner.
///
/// The loop deliberately has three distinct width bands:
///
/// - below the selected threshold, it delegates to the exact AVX2 scanner, so
///   an AVX-512 strategy never takes ownership of an unmeasured short range;
/// - at or above that threshold, it uses 64-byte ZMM loads and a 64-bit AVX-512
///   comparison mask;
/// - after a ZMM body, an in-range 16--63 byte tail uses AVX-512VL masks; the
///   remaining fewer than 16 bytes are scalar.
///
/// This keeps AVX-512 instructions restricted to a measured range without a
/// CPU-feature condition in the hot path. All vector loads are full in-bounds
/// loads; no speculative masked read crosses a Rust slice boundary.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,avx512f,avx512bw,avx512vl,ssse3")]
unsafe fn scan_avx512_bw_vl_hybrid(src: &[u8], lut: &Lut, zmm_min_bytes: usize) -> usize {
    use core::arch::x86_64::{
        __m128i, __m256i, __m512i, _mm256_and_si256, _mm256_broadcastsi128_si256,
        _mm256_cmpeq_epi8_mask, _mm256_loadu_si256, _mm256_set1_epi8, _mm256_setzero_si256,
        _mm256_shuffle_epi8, _mm256_srli_epi16, _mm512_and_si512, _mm512_broadcast_i32x4,
        _mm512_cmpeq_epi8_mask, _mm512_loadu_si512, _mm512_set1_epi8, _mm512_setzero_si512,
        _mm512_shuffle_epi8, _mm512_srli_epi16, _mm_and_si128, _mm_cmpeq_epi8_mask,
        _mm_loadu_si128, _mm_set1_epi8, _mm_setzero_si128, _mm_shuffle_epi8, _mm_srli_epi16,
    };

    const ZMM_LANES: usize = 64;
    const YMM_LANES: usize = 32;
    const XMM_LANES: usize = 16;
    debug_assert!(matches!(zmm_min_bytes, 64 | 128 | 256));

    if src.len() < zmm_min_bytes {
        // SAFETY: this function's complete target-feature contract includes
        // the AVX2+SSSE3 contract of the standard scanner. This exact fallback
        // prevents a selected AVX-512 strategy from changing an unadmitted
        // 16--255-byte path merely because the CPU can decode mask registers.
        return unsafe { scan_avx2(src, lut) };
    }

    let mut at = 0usize;
    // SAFETY: every vector load is guarded by the corresponding complete
    // width check, and all forms are explicitly unaligned. The function's
    // target-feature contract includes the AVX2, AVX-512F, AVX-512BW, and
    // AVX-512VL requirements of every intrinsic below.
    unsafe {
        let table_xmm = _mm_loadu_si128(lut.as_ptr().cast::<__m128i>());
        let bits_xmm = _mm_loadu_si128(BIT_LUT.as_ptr().cast::<__m128i>());
        let low_xmm = _mm_set1_epi8(0x0f);
        let zero_xmm = _mm_setzero_si128();

        let table_zmm = _mm512_broadcast_i32x4(table_xmm);
        let bits_zmm = _mm512_broadcast_i32x4(bits_xmm);
        let low_zmm = _mm512_set1_epi8(0x0f);
        let zero_zmm = _mm512_setzero_si512();
        while at + ZMM_LANES <= src.len() {
            let v = _mm512_loadu_si512(src.as_ptr().add(at).cast::<__m512i>());
            let lo = _mm512_and_si512(v, low_zmm);
            let hi = _mm512_and_si512(_mm512_srli_epi16::<4>(v), low_zmm);
            let hit = _mm512_and_si512(
                _mm512_shuffle_epi8(table_zmm, lo),
                _mm512_shuffle_epi8(bits_zmm, hi),
            );
            let stop = _mm512_cmpeq_epi8_mask(hit, zero_zmm);
            if stop != 0 {
                return at + stop.trailing_zeros() as usize;
            }
            at += ZMM_LANES;
        }

        // Build YMM values only if there is an in-bounds 32-byte block left.
        // On a 16--31 byte input this keeps the low-range path identical in
        // shape to `scan_avx2` rather than paying for a dead broadcast.
        if at + YMM_LANES <= src.len() {
            let table_ymm = _mm256_broadcastsi128_si256(table_xmm);
            let bits_ymm = _mm256_broadcastsi128_si256(bits_xmm);
            let low_ymm = _mm256_set1_epi8(0x0f);
            let zero_ymm = _mm256_setzero_si256();
            while at + YMM_LANES <= src.len() {
                let v = _mm256_loadu_si256(src.as_ptr().add(at).cast::<__m256i>());
                let lo = _mm256_and_si256(v, low_ymm);
                let hi = _mm256_and_si256(_mm256_srli_epi16::<4>(v), low_ymm);
                let hit = _mm256_and_si256(
                    _mm256_shuffle_epi8(table_ymm, lo),
                    _mm256_shuffle_epi8(bits_ymm, hi),
                );
                let stop = _mm256_cmpeq_epi8_mask(hit, zero_ymm) as u32;
                if stop != 0 {
                    return at + stop.trailing_zeros() as usize;
                }
                at += YMM_LANES;
            }
        }

        if at + XMM_LANES <= src.len() {
            let v = _mm_loadu_si128(src.as_ptr().add(at).cast::<__m128i>());
            let lo = _mm_and_si128(v, low_xmm);
            let hi = _mm_and_si128(_mm_srli_epi16::<4>(v), low_xmm);
            let hit = _mm_and_si128(
                _mm_shuffle_epi8(table_xmm, lo),
                _mm_shuffle_epi8(bits_xmm, hi),
            );
            let stop = _mm_cmpeq_epi8_mask(hit, zero_xmm) as u32;
            if stop != 0 {
                return at + stop.trailing_zeros() as usize;
            }
            at += XMM_LANES;
        }

        at + scan_scalar(&src[at..], lut)
    }
}

/// Exact long-run boundaries used by the one-time AVX-512 admission check.
/// They cover each change in the 64-byte body and the XMM/YMM tails without
/// pretending that 16--63 byte inputs execute ZMM work.
#[cfg(all(test, target_arch = "x86_64"))]
const AVX512_CALIBRATION_LENGTHS: [usize; 11] = [64, 65, 95, 96, 127, 128, 129, 255, 256, 257, 512];

/// Direct-kernel diagnostic calibration for the ignored benchmark. It never
/// runs in a shipping process and cannot publish an AVX-512 strategy. Each
/// measurement is a substantial batch and each A/B sample is split into
/// interleaved sub-batches to reduce scheduler and frequency drift.
#[cfg(all(test, target_arch = "x86_64"))]
const AVX512_CALIBRATION_SAMPLES: usize = 15;
#[cfg(all(test, target_arch = "x86_64"))]
// 65,536 calls per side make the shortest measured batch large enough for a
// p90 diagnostic, while keeping the ignored benchmark bounded.
const AVX512_CALIBRATION_ROUNDS: usize = 65_536;
#[cfg(all(test, target_arch = "x86_64"))]
const AVX512_CALIBRATION_SUB_BATCHES: usize = 8;

/// Returns a direct-kernel diagnostic threshold, or `None` when AVX2 remains
/// the safe result for this sample. Shipping dispatch deliberately ignores it.
#[cfg(all(test, target_arch = "x86_64"))]
fn avx512_zmm_threshold_for_diagnostic(features: CpuFeatures) -> Option<Avx512ZmmThreshold> {
    use std::hint::black_box;
    use std::time::{Duration, Instant};

    if !features.supports(CpuFeatures::AVX512_BW_VL) {
        return None;
    }

    fn median_and_p90(mut samples: [f64; AVX512_CALIBRATION_SAMPLES]) -> (f64, f64) {
        samples.sort_by(f64::total_cmp);
        let p90_index = (AVX512_CALIBRATION_SAMPLES * 9).div_ceil(10) - 1;
        (samples[AVX512_CALIBRATION_SAMPLES / 2], samples[p90_index])
    }

    unsafe fn measure(
        kernel: ScanStrategy,
        src: &[u8],
        lut: &Lut,
        len: usize,
        rounds: usize,
    ) -> Duration {
        let started = Instant::now();
        for _ in 0..rounds {
            // SAFETY: the caller supplied only the baseline AVX2 kernel or
            // the checked AVX-512BW+VL candidate after startup feature
            // detection established their target-feature contracts.
            let found = unsafe { kernel(black_box(&src[..len]), black_box(lut)) };
            debug_assert_eq!(found, len, "calibration buffer is one full run");
            black_box(found);
        }
        started.elapsed()
    }

    /// Measures both kernels in short alternating batches. The result includes
    /// the same total number of invocations on each side, but avoids treating a
    /// scheduler pause between two monolithic batches as a kernel regression.
    unsafe fn measure_interleaved_pair(
        baseline: ScanStrategy,
        candidate: ScanStrategy,
        src: &[u8],
        lut: &Lut,
        len: usize,
        phase: usize,
    ) -> (Duration, Duration) {
        let rounds = AVX512_CALIBRATION_ROUNDS / AVX512_CALIBRATION_SUB_BATCHES;
        let mut baseline_elapsed = Duration::ZERO;
        let mut candidate_elapsed = Duration::ZERO;
        let measure_checked = |kernel| {
            // SAFETY: the caller of this pair established the target-feature
            // contract for both supplied kernels before entering the loop.
            unsafe { measure(kernel, src, lut, len, rounds) }
        };
        for batch in 0..AVX512_CALIBRATION_SUB_BATCHES {
            if (phase + batch).is_multiple_of(2) {
                baseline_elapsed += measure_checked(baseline);
                candidate_elapsed += measure_checked(candidate);
            } else {
                candidate_elapsed += measure_checked(candidate);
                baseline_elapsed += measure_checked(baseline);
            }
        }
        (baseline_elapsed, candidate_elapsed)
    }

    let source = [0x01u8; 512];
    let lut = passthrough_lut(false, false, false);
    let baseline = scan_avx2 as ScanStrategy;
    let candidate = scan_avx512_bw_vl_from_64 as ScanStrategy;

    // Warm both bodies and their instruction-cache paths before paired timing.
    for &len in &AVX512_CALIBRATION_LENGTHS {
        // SAFETY: guarded by the complete feature set test above.
        unsafe {
            let _ = measure(baseline, &source, lut, len, AVX512_CALIBRATION_ROUNDS);
            let _ = measure(candidate, &source, lut, len, AVX512_CALIBRATION_ROUNDS);
        }
    }

    let mut ratios = [[0.0; AVX512_CALIBRATION_SAMPLES]; AVX512_CALIBRATION_LENGTHS.len()];
    for ((length_index, &len), ratios_for_length) in AVX512_CALIBRATION_LENGTHS
        .iter()
        .enumerate()
        .zip(ratios.iter_mut())
    {
        for (sample, ratio) in ratios_for_length.iter_mut().enumerate() {
            // Alternate every sub-batch, not only whole samples. Frequency
            // changes and a neighbouring process then affect both sides of
            // each paired observation.
            // SAFETY: the complete AVX-512 feature gate at this function's
            // entry establishes both supplied kernel contracts.
            let (baseline_time, candidate_time) = unsafe {
                measure_interleaved_pair(
                    baseline,
                    candidate,
                    &source,
                    lut,
                    len,
                    sample * AVX512_CALIBRATION_LENGTHS.len() + length_index,
                )
            };
            // Batches are intentionally long enough to make zero impossible;
            // retain the defensive clamp so a coarse clock fails closed rather
            // than accidentally admitting AVX-512.
            let baseline_ns = baseline_time.as_nanos().max(1) as f64;
            *ratio = candidate_time.as_nanos() as f64 / baseline_ns;
        }
    }

    for threshold in [
        Avx512ZmmThreshold::From64,
        Avx512ZmmThreshold::From128,
        Avx512ZmmThreshold::From256,
    ] {
        let admitted = AVX512_CALIBRATION_LENGTHS
            .iter()
            .enumerate()
            .filter(|(_, &len)| len >= threshold.bytes())
            .all(|(length_index, _)| {
                let (median, p90) = median_and_p90(ratios[length_index]);
                // 5% median is the product gate. Requiring the slowest 10%
                // not to lose avoids choosing a strategy whose nominal win is
                // a timing outlier or a frequency-transition artifact.
                median <= 0.95 && p90 <= 1.0
            });
        if admitted {
            return Some(threshold);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A byte no policy ever changes, for building runs of known length.
    const FILLER: u8 = 0x01;

    fn reset_call_counters() {
        SELECTED_WIDTH_SCAN_CALLS.with(|calls| calls.set(0));
        AVX_SSSE3_XMM_CALLS.with(|calls| calls.set(0));
    }

    fn selected_width_scan_calls() -> usize {
        SELECTED_WIDTH_SCAN_CALLS.with(std::cell::Cell::get)
    }

    fn avx_ssse3_xmm_calls() -> usize {
        AVX_SSSE3_XMM_CALLS.with(std::cell::Cell::get)
    }

    /// Every kernel this machine can actually run, named for assertion
    /// messages. The scalar reference is always first.
    fn kernels() -> Vec<(&'static str, ScanStrategy)> {
        #[allow(unused_mut)]
        let mut all: Vec<(&'static str, ScanStrategy)> =
            vec![("scalar", scan_scalar as ScanStrategy)];
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx") && is_x86_feature_detected!("ssse3") {
                all.push(("avx-ssse3-128", scan_avx_ssse3_128 as ScanStrategy));
            }
            if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("ssse3") {
                all.push(("avx2-hybrid", scan_avx2 as ScanStrategy));
            }
            if is_x86_feature_detected!("avx512f")
                && is_x86_feature_detected!("avx512bw")
                && is_x86_feature_detected!("avx512vl")
            {
                all.push((
                    "avx512bw-vl-from-64",
                    scan_avx512_bw_vl_from_64 as ScanStrategy,
                ));
                all.push((
                    "avx512bw-vl-from-128",
                    scan_avx512_bw_vl_from_128 as ScanStrategy,
                ));
                all.push((
                    "avx512bw-vl-from-256",
                    scan_avx512_bw_vl_from_256 as ScanStrategy,
                ));
            }
        }
        all
    }

    /// All eight policies, as the `(full_alpha, full_digit, full_symbol)`
    /// triples the tables are built from.
    fn policies() -> impl Iterator<Item = (bool, bool, bool)> {
        (0..8).map(|i| (i & 0b100 != 0, i & 0b010 != 0, i & 0b001 != 0))
    }

    /// The agreement tests are only as broad as the kernels this machine can
    /// run, so a passing run says less on an old CPU than on a new one. This
    /// puts the list in the output (`--nocapture`) and holds the floor the
    /// build already assumes: compiled with `+avx`, so a machine that ran
    /// this test at all has AVX.
    #[test]
    fn the_kernels_under_test_are_named() {
        let names: Vec<&str> = kernels().iter().map(|(name, _)| *name).collect();
        let selected = startup().expect("the test host meets the shipped compatibility floor");
        println!(
            "kernels under test: {names:?} (resolved width scan {})",
            selected.width_scan().metadata().name
        );
        assert!(names.contains(&"scalar"));
        #[cfg(target_arch = "x86_64")]
        assert!(
            names.contains(&"avx-ssse3-128"),
            "this binary is built with the AVX+SSSE3 compatibility floor"
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn synthetic_features_resolve_one_concrete_kernel_set() {
        let missing_ssse3 = CpuFeatures::synthetic(true, false, false, false, false, false);
        assert!(resolve_kernel_set(missing_ssse3, None).is_err());
        let avx2_without_ssse3 = CpuFeatures::synthetic(true, false, true, false, false, false);
        assert!(resolve_kernel_set(avx2_without_ssse3, None).is_err());

        let avx_floor = CpuFeatures::synthetic(true, true, false, false, false, false);
        assert_eq!(
            resolve_kernel_set(avx_floor, None)
                .expect("AVX+SSSE3")
                .width_scan()
                .metadata()
                .id,
            WidthScanStrategyId::AvxSsse3Xmm
        );

        let avx2 = CpuFeatures::synthetic(true, true, true, false, false, false);
        assert_eq!(
            resolve_kernel_set(avx2, None)
                .expect("AVX2")
                .width_scan()
                .metadata()
                .id,
            WidthScanStrategyId::Avx2Hybrid
        );

        let avx512_bw_vl = CpuFeatures::synthetic(true, true, true, true, true, true);
        assert_eq!(
            resolve_kernel_set(avx512_bw_vl, None)
                .expect("an AVX-512 CPU keeps AVX2 without an admission result")
                .width_scan()
                .metadata()
                .id,
            WidthScanStrategyId::Avx2Hybrid
        );
        assert_eq!(
            resolve_kernel_set(avx512_bw_vl, Some(Avx512ZmmThreshold::From64))
                .expect("complete AVX-512BW+VL capability set")
                .width_scan()
                .metadata()
                .id,
            WidthScanStrategyId::Avx512BwVlFrom64
        );
        assert_eq!(
            resolve_kernel_set(avx512_bw_vl, Some(Avx512ZmmThreshold::From128))
                .expect("complete AVX-512BW+VL capability set")
                .width_scan()
                .metadata()
                .id,
            WidthScanStrategyId::Avx512BwVlFrom128
        );
        assert_eq!(
            resolve_kernel_set(avx512_bw_vl, Some(Avx512ZmmThreshold::From256))
                .expect("complete AVX-512BW+VL capability set")
                .width_scan()
                .metadata()
                .id,
            WidthScanStrategyId::Avx512BwVlFrom256
        );

        let missing_vl = CpuFeatures::synthetic(true, true, true, true, true, false);
        assert_eq!(
            resolve_kernel_set(missing_vl, Some(Avx512ZmmThreshold::From64))
                .expect("incomplete AVX-512 must fall back to AVX2")
                .width_scan()
                .metadata()
                .id,
            WidthScanStrategyId::Avx2Hybrid
        );
    }

    #[test]
    fn shipping_startup_keeps_avx512_candidates_bench_only() {
        let selected = startup().expect("the shipped compatibility floor");
        assert!(
            !matches!(
                selected.width_scan().metadata().id,
                WidthScanStrategyId::Avx512BwVlFrom64
                    | WidthScanStrategyId::Avx512BwVlFrom128
                    | WidthScanStrategyId::Avx512BwVlFrom256
            ),
            "the release startup resolver must not publish a bench-only AVX-512 candidate"
        );
    }

    #[test]
    fn short_inputs_skip_the_selected_strategy_call() {
        let lut = passthrough_lut(false, false, false);
        reset_call_counters();

        let short = [FILLER; MIN_VECTOR_BYTES - 1];
        assert_eq!(passthrough_len(&short, lut), short.len());
        assert_eq!(
            selected_width_scan_calls(),
            0,
            "0--15 byte input must take the caller's scalar path"
        );

        let vector_sized = [FILLER; MIN_VECTOR_BYTES];
        assert_eq!(passthrough_len(&vector_sized, lut), vector_sized.len());
        assert_eq!(
            selected_width_scan_calls(),
            1,
            "16-byte input should make exactly one selected-strategy call"
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn avx2_handles_its_xmm_tail_without_entering_the_avx_floor_kernel() {
        if !(is_x86_feature_detected!("avx2") && is_x86_feature_detected!("ssse3")) {
            return;
        }

        let lut = passthrough_lut(false, false, false);
        for len in [16usize, 17, 31, 32, 33, 47, 48, 63] {
            reset_call_counters();
            let buffer = vec![FILLER; len];
            // SAFETY: guarded by this test's CPUID check.
            let actual = unsafe { scan_avx2(&buffer, lut) };
            assert_eq!(actual, len, "AVX2 result for {len} bytes");
            assert_eq!(
                avx_ssse3_xmm_calls(),
                0,
                "AVX2 must finish its {len}-byte input without a lower-tier call"
            );
        }
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn avx512_hybrids_delegate_unmeasured_ranges_to_avx2_and_use_zmm_at_threshold() {
        if !(is_x86_feature_detected!("avx512f")
            && is_x86_feature_detected!("avx512bw")
            && is_x86_feature_detected!("avx512vl"))
        {
            return;
        }

        let lut = passthrough_lut(false, false, false);
        for (len, expected) in [
            (
                16usize,
                Avx512PathCounts {
                    avx2_ymm_blocks: 0,
                    avx2_xmm_blocks: 1,
                    zmm_blocks: 0,
                    vl_ymm_blocks: 0,
                    vl_xmm_blocks: 0,
                },
            ),
            (
                31,
                Avx512PathCounts {
                    avx2_ymm_blocks: 0,
                    avx2_xmm_blocks: 1,
                    zmm_blocks: 0,
                    vl_ymm_blocks: 0,
                    vl_xmm_blocks: 0,
                },
            ),
            (
                32,
                Avx512PathCounts {
                    avx2_ymm_blocks: 1,
                    avx2_xmm_blocks: 0,
                    zmm_blocks: 0,
                    vl_ymm_blocks: 0,
                    vl_xmm_blocks: 0,
                },
            ),
            (
                63,
                Avx512PathCounts {
                    avx2_ymm_blocks: 1,
                    avx2_xmm_blocks: 1,
                    zmm_blocks: 0,
                    vl_ymm_blocks: 0,
                    vl_xmm_blocks: 0,
                },
            ),
            (
                64,
                Avx512PathCounts {
                    avx2_ymm_blocks: 0,
                    avx2_xmm_blocks: 0,
                    zmm_blocks: 1,
                    vl_ymm_blocks: 0,
                    vl_xmm_blocks: 0,
                },
            ),
            (
                65,
                Avx512PathCounts {
                    avx2_ymm_blocks: 0,
                    avx2_xmm_blocks: 0,
                    zmm_blocks: 1,
                    vl_ymm_blocks: 0,
                    vl_xmm_blocks: 0,
                },
            ),
            (
                95,
                Avx512PathCounts {
                    avx2_ymm_blocks: 0,
                    avx2_xmm_blocks: 0,
                    zmm_blocks: 1,
                    vl_ymm_blocks: 0,
                    vl_xmm_blocks: 1,
                },
            ),
            (
                96,
                Avx512PathCounts {
                    avx2_ymm_blocks: 0,
                    avx2_xmm_blocks: 0,
                    zmm_blocks: 1,
                    vl_ymm_blocks: 1,
                    vl_xmm_blocks: 0,
                },
            ),
            (
                127,
                Avx512PathCounts {
                    avx2_ymm_blocks: 0,
                    avx2_xmm_blocks: 0,
                    zmm_blocks: 1,
                    vl_ymm_blocks: 1,
                    vl_xmm_blocks: 1,
                },
            ),
            (
                128,
                Avx512PathCounts {
                    avx2_ymm_blocks: 0,
                    avx2_xmm_blocks: 0,
                    zmm_blocks: 2,
                    vl_ymm_blocks: 0,
                    vl_xmm_blocks: 0,
                },
            ),
        ] {
            reset_call_counters();
            let buffer = vec![FILLER; len];
            // SAFETY: guarded by the complete feature check above.
            let found = unsafe { scan_avx512_bw_vl_from_64(&buffer, lut) };
            assert_eq!(found, len, "AVX-512 result for {len} bytes");
            assert_eq!(
                avx512_path_counts_for_scan(&buffer, lut, Avx512ZmmThreshold::From64.bytes()),
                expected,
                "unexpected AVX-512 path for {len} bytes"
            );
            assert_eq!(
                avx_ssse3_xmm_calls(),
                0,
                "the AVX-512 hybrid must not delegate to the AVX floor"
            );
        }

        reset_call_counters();
        let len = 127;
        let buffer = vec![FILLER; len];
        // SAFETY: as above.
        assert_eq!(unsafe { scan_avx512_bw_vl_from_128(&buffer, lut) }, len);
        assert_eq!(
            avx512_path_counts_for_scan(&buffer, lut, Avx512ZmmThreshold::From128.bytes()),
            Avx512PathCounts {
                avx2_ymm_blocks: 3,
                avx2_xmm_blocks: 1,
                zmm_blocks: 0,
                vl_ymm_blocks: 0,
                vl_xmm_blocks: 0,
            },
            "the 128-byte strategy delegates 127 bytes to the exact AVX2 body"
        );

        reset_call_counters();
        let len = 256;
        let buffer = vec![FILLER; len];
        // SAFETY: as above.
        assert_eq!(unsafe { scan_avx512_bw_vl_from_256(&buffer, lut) }, len);
        assert_eq!(
            avx512_path_counts_for_scan(&buffer, lut, Avx512ZmmThreshold::From256.bytes()),
            Avx512PathCounts {
                avx2_ymm_blocks: 0,
                avx2_xmm_blocks: 0,
                zmm_blocks: 4,
                vl_ymm_blocks: 0,
                vl_xmm_blocks: 0
            },
            "the 256-byte strategy starts ZMM work only at 256 bytes"
        );
    }

    /// The tables are generated, so what needs proving is that generating
    /// them preserved the meaning of the predicate they came from — for
    /// every byte, including the non-ASCII ones the tables must reject.
    #[test]
    fn the_tables_agree_with_the_predicate_for_every_byte() {
        for (alpha, digit, symbol) in policies() {
            let lut = passthrough_lut(alpha, digit, symbol);
            for b in 0..=u8::MAX {
                let expected = b < 0x80 && passes_through(b, alpha, digit, symbol);
                assert_eq!(
                    admits(lut, b),
                    expected,
                    "byte {b:#04x} under ({alpha}, {digit}, {symbol})"
                );
            }
        }
    }

    /// The one property the normalizer depends on for its slicing to be
    /// sound: a run never extends into a multi-byte character.
    #[test]
    fn no_table_ever_admits_a_non_ascii_byte() {
        for (alpha, digit, symbol) in policies() {
            let lut = passthrough_lut(alpha, digit, symbol);
            for b in 0x80..=u8::MAX {
                assert!(!admits(lut, b), "byte {b:#04x} escaped the ASCII guard");
            }
        }
    }

    /// A stopper at every position of a long buffer, which walks the answer
    /// across every block boundary each kernel has — 16, 32 and 64 — and
    /// through every tail length.
    #[test]
    fn kernels_find_a_stopper_at_every_position() {
        let lut = passthrough_lut(false, false, false);
        let stopper = 0xE3; // the lead byte of a kana character
        for len in [1usize, 15, 16, 17, 31, 32, 33, 63, 64, 65, 130, 257] {
            for at in 0..len {
                let mut buffer = vec![FILLER; len];
                buffer[at] = stopper;
                for (name, kernel) in kernels() {
                    // SAFETY: `kernels` returns only kernels whose features
                    // this machine was just measured to have.
                    let found = unsafe { kernel(&buffer, lut) };
                    assert_eq!(found, at, "{name} on len {len} with a stopper at {at}");
                }
            }
        }
    }

    /// The other half of the boundary walk: nothing to stop on, so every
    /// kernel has to run out its main loop and its tail and agree on the
    /// total.
    #[test]
    fn an_unbroken_run_is_reported_whole() {
        let lut = passthrough_lut(false, false, false);
        for len in 0..=300usize {
            let buffer = vec![FILLER; len];
            for (name, kernel) in kernels() {
                // SAFETY: as above.
                let found = unsafe { kernel(&buffer, lut) };
                assert_eq!(found, len, "{name} on an unbroken run of {len}");
            }
        }
    }

    /// Every byte value, under every policy, at a position past the widest
    /// block — so a kernel that mis-classifies one value cannot hide behind
    /// a tail that happened to be scalar.
    #[test]
    fn every_byte_value_is_classified_the_same_by_every_kernel() {
        for (alpha, digit, symbol) in policies() {
            let lut = passthrough_lut(alpha, digit, symbol);
            for b in 0..=u8::MAX {
                let mut buffer = vec![FILLER; 96];
                buffer[70] = b;
                let expected = scan_scalar(&buffer, lut);
                for (name, kernel) in kernels() {
                    // SAFETY: as above.
                    let found = unsafe { kernel(&buffer, lut) };
                    assert_eq!(
                        found, expected,
                        "{name} disagreed on byte {b:#04x} under ({alpha}, {digit}, {symbol})"
                    );
                }
            }
        }
    }

    /// Pseudo-random buffers, because the hand-built cases above all have
    /// one stopper in a field of filler and real text does not.
    #[test]
    fn kernels_agree_with_the_reference_on_a_random_corpus() {
        let mut rng = Lcg::seeded(0x5A6B_1234_9876_0001);
        for (alpha, digit, symbol) in policies() {
            let lut = passthrough_lut(alpha, digit, symbol);
            for len in 0..=200usize {
                let buffer: Vec<u8> = (0..len).map(|_| rng.text_byte()).collect();
                let expected = scan_scalar(&buffer, lut);
                for (name, kernel) in kernels() {
                    // SAFETY: as above.
                    let found = unsafe { kernel(&buffer, lut) };
                    assert_eq!(
                        found, expected,
                        "{name} disagreed on a random buffer of {len} under \
                         ({alpha}, {digit}, {symbol}): {buffer:?}"
                    );
                }
            }
        }
    }

    /// The public entry point has its own short-input path, so it needs its
    /// own agreement test rather than inheriting the kernels'.
    #[test]
    fn the_dispatched_entry_point_agrees_with_the_reference() {
        let mut rng = Lcg::seeded(0x0BAD_C0DE_1234_5678);
        for (alpha, digit, symbol) in policies() {
            let lut = passthrough_lut(alpha, digit, symbol);
            for len in 0..=120usize {
                let buffer: Vec<u8> = (0..len).map(|_| rng.text_byte()).collect();
                assert_eq!(
                    passthrough_len(&buffer, lut),
                    scan_scalar(&buffer, lut),
                    "dispatch disagreed on {buffer:?} under ({alpha}, {digit}, {symbol})"
                );
            }
        }
    }

    /// The process-selected AVX-512 candidate, or the conservative 256-byte
    /// bench-only candidate used while shipping startup deliberately retains
    /// AVX2. The printed path names the executed candidate rather than implying
    /// that production dispatch uses it.
    #[cfg(target_arch = "x86_64")]
    fn avx512_candidate_for_benchmark(
        selected: KernelSet,
    ) -> (&'static str, Avx512ZmmThreshold, ScanStrategy) {
        match selected.width_scan().metadata().id {
            WidthScanStrategyId::Avx512BwVlFrom64 => (
                "avx512bw-vl-from-64",
                Avx512ZmmThreshold::From64,
                scan_avx512_bw_vl_from_64 as ScanStrategy,
            ),
            WidthScanStrategyId::Avx512BwVlFrom128 => (
                "avx512bw-vl-from-128",
                Avx512ZmmThreshold::From128,
                scan_avx512_bw_vl_from_128 as ScanStrategy,
            ),
            WidthScanStrategyId::Avx512BwVlFrom256 => (
                "avx512bw-vl-from-256",
                Avx512ZmmThreshold::From256,
                scan_avx512_bw_vl_from_256 as ScanStrategy,
            ),
            // Production currently declines every AVX-512 candidate. A
            // conservative 256-byte takeover is still useful direct evidence,
            // but the label makes its bench-only status explicit.
            WidthScanStrategyId::Scalar
            | WidthScanStrategyId::AvxSsse3Xmm
            | WidthScanStrategyId::Avx2Hybrid => (
                "avx512bw-vl-from-256 (bench-only candidate)",
                Avx512ZmmThreshold::From256,
                scan_avx512_bw_vl_from_256 as ScanStrategy,
            ),
        }
    }

    /// Direct kernel measurements are deliberately separate from the
    /// end-to-end normalizer benchmark. They compare AVX-512BW+VL directly to
    /// its AVX2 baseline using interleaved pairs and print p10/p50/p90 of the
    /// candidate/baseline ratio. The executed-path column makes it explicit
    /// when a short input took the exact AVX2 fallback rather than any
    /// AVX-512 instruction path.
    #[test]
    #[ignore = "timing, not a threshold: run with --release --ignored --nocapture and read it"]
    fn direct_ascii_passthrough_kernel_benchmark() {
        #[cfg(not(target_arch = "x86_64"))]
        {
            println!("direct AVX2/AVX-512 benchmark is unavailable off x86-64");
            return;
        }

        #[cfg(target_arch = "x86_64")]
        direct_ascii_passthrough_kernel_benchmark_x86_64();
    }

    #[cfg(target_arch = "x86_64")]
    fn direct_ascii_passthrough_kernel_benchmark_x86_64() {
        use std::hint::black_box;
        use std::time::Instant;

        // Keep even the shortest 0--16-byte sub-batch near the millisecond
        // range on a fast desktop CPU. A 5% decision made from a ten-microsecond
        // timer interval is mostly a scheduler measurement, not a kernel one.
        const ROUNDS: usize = 5_000_000;
        const SAMPLES: usize = 15;
        const INTERLEAVED_SUB_BATCHES: usize = 10;
        const LENGTHS: &[usize] = &[
            0, 1, 7, 8, 15, 16, 17, 31, 32, 33, 47, 48, 63, 64, 65, 95, 96, 127, 128, 129, 255,
            256, 257, 512,
        ];

        fn percentile(mut samples: [f64; SAMPLES], percent: usize) -> f64 {
            samples.sort_by(f64::total_cmp);
            let index = (SAMPLES * percent).div_ceil(100).saturating_sub(1);
            samples[index]
        }

        unsafe fn nanos_each(kernel: ScanStrategy, src: &[u8], lut: &Lut, rounds: usize) -> f64 {
            let started = Instant::now();
            for _ in 0..rounds {
                // SAFETY: the caller established the kernel's complete
                // target-feature contract before entering this benchmark.
                let found = unsafe { kernel(black_box(src), black_box(lut)) };
                black_box(found);
            }
            started.elapsed().as_secs_f64() * 1e9 / rounds as f64
        }

        unsafe fn nanos_interleaved_pair(
            baseline: ScanStrategy,
            candidate: ScanStrategy,
            src: &[u8],
            lut: &Lut,
            phase: usize,
        ) -> (f64, f64) {
            let rounds = ROUNDS / INTERLEAVED_SUB_BATCHES;
            let mut baseline_ns = 0.0;
            let mut candidate_ns = 0.0;
            let measure_checked = |kernel| {
                // SAFETY: the enclosing feature check established the complete
                // target-feature contract for both benchmark kernels.
                unsafe { nanos_each(kernel, src, lut, rounds) }
            };
            for batch in 0..INTERLEAVED_SUB_BATCHES {
                if (phase + batch).is_multiple_of(2) {
                    baseline_ns += measure_checked(baseline);
                    candidate_ns += measure_checked(candidate);
                } else {
                    candidate_ns += measure_checked(candidate);
                    baseline_ns += measure_checked(baseline);
                }
            }
            (
                baseline_ns / INTERLEAVED_SUB_BATCHES as f64,
                candidate_ns / INTERLEAVED_SUB_BATCHES as f64,
            )
        }

        if !(is_x86_feature_detected!("avx2")
            && is_x86_feature_detected!("ssse3")
            && is_x86_feature_detected!("avx512f")
            && is_x86_feature_detected!("avx512bw")
            && is_x86_feature_detected!("avx512vl"))
        {
            println!(
                "direct AVX2/AVX-512 benchmark skipped: this CPU lacks AVX2 or complete AVX-512F+BW+VL"
            );
            return;
        }

        let lut = passthrough_lut(false, false, false);
        let selected = startup().expect("the benchmark requires the compatibility floor");
        let (candidate_name, candidate_threshold, candidate) =
            avx512_candidate_for_benchmark(selected);
        let diagnostic_threshold = avx512_zmm_threshold_for_diagnostic(
            cpu::detect_at_startup().expect("the benchmark requires CPU detection"),
        );
        println!(
            "\ndirect ASCII pass-through scanner: resolved={} ({})",
            selected.width_scan().metadata().name,
            selected.width_scan().metadata().benchmark_id,
        );
        println!(
            "candidate={candidate_name}; a2*=exact AVX2 fallback, while z/v*=actual AVX-512 work."
        );
        println!(
            "direct admission diagnostic={diagnostic_threshold:?} (bench-only; production remains AVX2)."
        );
        println!(
            "{:<6} {:<28} {:>11} {:>11} {:>9} {:>9} {:>9}",
            "bytes",
            "candidate path",
            "avx2 p50",
            "avx512 p50",
            "ratio p10",
            "ratio p50",
            "ratio p90"
        );

        for &len in LENGTHS {
            let src = vec![FILLER; len];
            let baseline = scan_avx2 as ScanStrategy;

            // A small warm-up makes the samples describe the two bodies rather
            // than their first instruction-cache fill.
            for _ in 0..ROUNDS / 20 {
                // SAFETY: established by this function's feature check.
                unsafe {
                    black_box(baseline(src.as_slice(), lut));
                    black_box(candidate(src.as_slice(), lut));
                }
            }

            let mut baseline_ns = [0.0; SAMPLES];
            let mut candidate_ns = [0.0; SAMPLES];
            let mut ratios = [0.0; SAMPLES];
            for sample in 0..SAMPLES {
                // Interleave short batches inside every sample so a scheduler
                // pause or transient frequency change is shared by both sides.
                // SAFETY: this function's complete feature gate establishes
                // the target-feature contracts for both raw function pointers.
                let (base, candidate_time) = unsafe {
                    nanos_interleaved_pair(baseline, candidate, src.as_slice(), lut, sample)
                };
                baseline_ns[sample] = base;
                candidate_ns[sample] = candidate_time;
                ratios[sample] = candidate_time / base.max(f64::MIN_POSITIVE);
            }

            // SAFETY: the feature check above establishes the contract.
            let found = unsafe { candidate(&src, lut) };
            assert_eq!(found, len, "benchmark input must be one full run");
            let path = avx512_path_counts_for_scan(&src, lut, candidate_threshold.bytes());
            let path = format_avx512_path(path);
            println!(
                "{len:<6} {path:<28} {base:>11.2} {candidate_time:>11.2} {p10:>8.3}x {p50:>8.3}x {p90:>8.3}x",
                base = percentile(baseline_ns, 50),
                candidate_time = percentile(candidate_ns, 50),
                p10 = percentile(ratios, 10),
                p50 = percentile(ratios, 50),
                p90 = percentile(ratios, 90),
            );
        }

        println!(
            "Admission rule: for every exact length a threshold owns, ratio p50 <= 0.950 and p90 <= 1.000; otherwise AVX2 remains selected."
        );
    }

    /// End-to-end counterpart to the direct scanner benchmark above. It keeps
    /// the whole normalizer and sink write in the measurement, while injecting
    /// the concrete scanner through a test-only, non-global seam. This avoids
    /// making tests race over the process-wide selected strategy.
    #[cfg(target_arch = "x86_64")]
    #[test]
    #[ignore = "timing, not a threshold: run with --release --ignored --nocapture and read it"]
    fn end_to_end_normalizer_avx512_vs_avx2_benchmark() {
        use crate::width::{Normalizer, PunctuationStyle, Width, WidthPolicy};
        use sakura_proto::{FixedStr, Mode};
        use std::hint::black_box;
        use std::time::Instant;

        type Sink = FixedStr<2_048>;
        const SAMPLES: usize = 15;
        const PROBE_ROUNDS: usize = 4_096;
        const MIN_SAMPLE_ROUNDS: usize = 512;
        const MAX_SAMPLE_ROUNDS: usize = 500_000;
        const TARGET_SIDE_SAMPLE_NS: f64 = 10_000_000.0;
        const PAIR_CHUNK_ROUNDS: usize = 4_096;

        if !(is_x86_feature_detected!("avx2")
            && is_x86_feature_detected!("ssse3")
            && is_x86_feature_detected!("avx512f")
            && is_x86_feature_detected!("avx512bw")
            && is_x86_feature_detected!("avx512vl"))
        {
            println!(
                "end-to-end AVX2/AVX-512 benchmark skipped: this CPU lacks AVX2 or complete AVX-512F+BW+VL"
            );
            return;
        }

        fn percentile(mut samples: [f64; SAMPLES], percent: usize) -> f64 {
            samples.sort_by(f64::total_cmp);
            let index = (SAMPLES * percent).div_ceil(100).saturating_sub(1);
            samples[index]
        }

        unsafe fn normalize_with(normalizer: &Normalizer, src: &str, kernel: ScanStrategy) -> Sink {
            let mut dst = Sink::new();
            // SAFETY: the benchmark's feature gate establishes the raw
            // target-feature function pointer's contract.
            unsafe { normalizer.normalize_into_with_scan(src, Mode::Hiragana, &mut dst, kernel) }
                .expect("the benchmark sink is sized for its corpora");
            dst
        }

        unsafe fn nanos_each(
            normalizer: &Normalizer,
            src: &str,
            kernel: ScanStrategy,
            rounds: usize,
        ) -> f64 {
            let started = Instant::now();
            for _ in 0..rounds {
                // SAFETY: forwarded from the caller's feature gate.
                let dst = unsafe { normalize_with(normalizer, black_box(src), kernel) };
                black_box(dst.len());
            }
            started.elapsed().as_secs_f64() * 1e9 / rounds as f64
        }

        /// Chooses an equal, per-kernel sample size for one corpus. The probe
        /// is outside the paired samples; it only prevents a 25-ns keystroke
        /// and a multi-microsecond full-width rewrite from using equally noisy
        /// timer intervals. The clamp keeps the ignored benchmark bounded.
        unsafe fn rounds_per_sample(
            normalizer: &Normalizer,
            src: &str,
            baseline: ScanStrategy,
        ) -> usize {
            // SAFETY: forwarded from the enclosing feature gate.
            let per_call_ns = unsafe { nanos_each(normalizer, src, baseline, PROBE_ROUNDS) };
            let wanted =
                (TARGET_SIDE_SAMPLE_NS / per_call_ns.max(f64::MIN_POSITIVE)).ceil() as usize;
            wanted.clamp(MIN_SAMPLE_ROUNDS, MAX_SAMPLE_ROUNDS)
        }

        unsafe fn nanos_interleaved_pair(
            normalizer: &Normalizer,
            src: &str,
            baseline: ScanStrategy,
            candidate: ScanStrategy,
            rounds: usize,
            phase: usize,
        ) -> (f64, f64) {
            let mut baseline_total_ns = 0.0;
            let mut candidate_total_ns = 0.0;
            let mut remaining = rounds;
            let mut chunk_index = 0;
            let measure_checked = |kernel, count| {
                // SAFETY: the caller established the full target-feature
                // contract for both raw kernel pointers before pairing them.
                unsafe { nanos_each(normalizer, src, kernel, count) }
            };
            while remaining != 0 {
                let chunk_rounds = remaining.min(PAIR_CHUNK_ROUNDS);
                // Alternating at this finer granularity makes each side
                // experience the same short-lived scheduler and frequency
                // changes.
                if (phase + chunk_index).is_multiple_of(2) {
                    baseline_total_ns +=
                        measure_checked(baseline, chunk_rounds) * chunk_rounds as f64;
                    candidate_total_ns +=
                        measure_checked(candidate, chunk_rounds) * chunk_rounds as f64;
                } else {
                    candidate_total_ns +=
                        measure_checked(candidate, chunk_rounds) * chunk_rounds as f64;
                    baseline_total_ns +=
                        measure_checked(baseline, chunk_rounds) * chunk_rounds as f64;
                }
                remaining -= chunk_rounds;
                chunk_index += 1;
            }
            (
                baseline_total_ns / rounds as f64,
                candidate_total_ns / rounds as f64,
            )
        }

        let long_ascii = "docker compose up -d --build --remove-orphans ".repeat(12);
        let cases = [
            ("one keystroke", String::from("k")),
            (
                "ascii 45",
                String::from("docker compose up -d --build --remove-orphans"),
            ),
            ("ascii 512", long_ascii),
            (
                "japanese prose",
                String::from("日本語の文章は幅ポリシーのASCII走査対象ではありません。"),
            ),
            (
                "mixed",
                String::from("Docker のビルドには --cache-from を指定して、再現性を確認します。"),
            ),
        ];
        let half = Normalizer::default();
        let full = Normalizer {
            width: WidthPolicy {
                alnum: Width::Full,
                number: Width::Full,
                symbol: Width::Full,
            },
            punctuation: PunctuationStyle::KutenTouten,
            brackets: crate::width::BracketStyle::default(),
        };
        let baseline = scan_avx2 as ScanStrategy;
        let selected = startup().expect("the benchmark requires the compatibility floor");
        let (candidate_name, candidate_threshold, candidate) =
            avx512_candidate_for_benchmark(selected);

        println!("benchmark candidate={candidate_name}");
        println!(
            "\nend-to-end normalizer AVX2 ↔ AVX-512BW+VL pairs (the global dispatch pointer is common work and intentionally excluded):"
        );
        println!(
            "{:<16} {:<14} {:<28} {:>8} {:>11} {:>11} {:>9} {:>9} {:>9}",
            "corpus",
            "policy",
            "candidate path",
            "calls/s",
            "avx2 p50",
            "avx512 p50",
            "ratio p10",
            "ratio p50",
            "ratio p90"
        );

        for (corpus, src) in &cases {
            for (policy, normalizer, lut) in [
                ("half (pass)", &half, passthrough_lut(false, false, false)),
                ("full (rewrite)", &full, passthrough_lut(true, true, true)),
            ] {
                // Confirm the end-to-end outputs before timing either body.
                // SAFETY: this benchmark's feature gate establishes the raw
                // kernel contracts for both normalizer executions.
                let (baseline_output, candidate_output) = unsafe {
                    (
                        normalize_with(normalizer, src, baseline),
                        normalize_with(normalizer, src, candidate),
                    )
                };
                assert_eq!(
                    baseline_output.as_str(),
                    candidate_output.as_str(),
                    "normalizer output mismatch for {corpus} under {policy}"
                );

                // SAFETY: this benchmark's feature gate establishes the raw
                // kernel function-pointer contracts.
                let sample_rounds = unsafe { rounds_per_sample(normalizer, src, baseline) };
                for _ in 0..(sample_rounds / 4) {
                    // SAFETY: this benchmark's feature gate establishes the
                    // raw kernel contracts for both warm-up executions.
                    unsafe {
                        black_box(normalize_with(normalizer, src, baseline));
                        black_box(normalize_with(normalizer, src, candidate));
                    }
                }

                let mut baseline_ns = [0.0; SAMPLES];
                let mut candidate_ns = [0.0; SAMPLES];
                let mut ratios = [0.0; SAMPLES];
                for sample in 0..SAMPLES {
                    // Interleave short batches inside every sample so the
                    // normalizer's non-SIMD work is shared fairly as well.
                    // SAFETY: this benchmark's feature gate establishes the
                    // raw kernel contracts for both paired executions.
                    let (base, candidate_time) = unsafe {
                        nanos_interleaved_pair(
                            normalizer,
                            src,
                            baseline,
                            candidate,
                            sample_rounds,
                            sample,
                        )
                    };
                    baseline_ns[sample] = base;
                    candidate_ns[sample] = candidate_time;
                    ratios[sample] = candidate_time / base.max(f64::MIN_POSITIVE);
                }

                let path =
                    avx512_path_counts_for_normalizer_runs(src, lut, candidate_threshold.bytes());
                let path = format_avx512_path(path);
                println!(
                    "{corpus:<16} {policy:<14} {path:<28} {sample_rounds:>8} {base:>11.2} {candidate_time:>11.2} {p10:>8.3}x {p50:>8.3}x {p90:>8.3}x",
                    base = percentile(baseline_ns, 50),
                    candidate_time = percentile(candidate_ns, 50),
                    p10 = percentile(ratios, 10),
                    p50 = percentile(ratios, 50),
                    p90 = percentile(ratios, 90),
                );
            }
        }
        println!(
            "Japanese and full-width-rewrite rows are regression observations, not AVX-512 performance claims: their path should normally be a2*=0 and z/v*=0."
        );
    }

    /// Each policy has to actually govern its own channel and nothing else,
    /// or the tables would be eight copies of the same set.
    #[test]
    fn each_channel_stops_only_its_own_characters() {
        let alpha_only = passthrough_lut(true, false, false);
        assert!(!admits(alpha_only, b'a'));
        assert!(admits(alpha_only, b'0'));
        assert!(admits(alpha_only, b'@'));

        let digit_only = passthrough_lut(false, true, false);
        assert!(admits(digit_only, b'a'));
        assert!(!admits(digit_only, b'0'));
        assert!(admits(digit_only, b'@'));

        let symbol_only = passthrough_lut(false, false, true);
        assert!(admits(symbol_only, b'a'));
        assert!(admits(symbol_only, b'0'));
        assert!(!admits(symbol_only, b'@'));
        assert!(
            admits(symbol_only, b' '),
            "space is owned by SpaceWidth, not the symbol channel"
        );

        // Control characters are outside the policy in every combination.
        let everything = passthrough_lut(true, true, true);
        assert!(admits(everything, b'\n'));
        assert!(admits(everything, 0x7F));
        assert!(!admits(everything, b'~'));
    }

    /// No `rand` crate — the dependency policy (DESIGN 3.1) permits none, and
    /// a fixed multiplier makes a failure reproducible anyway.
    #[derive(Debug)]
    struct Lcg(u64);

    impl Lcg {
        fn seeded(seed: u64) -> Self {
            Lcg(seed)
        }

        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }

        /// A byte from a distribution that looks more like text than a
        /// uniform draw would: uniform bytes alone are non-ASCII half the
        /// time, so runs would almost never reach a vector register.
        fn text_byte(&mut self) -> u8 {
            let r = self.next();
            match r % 8 {
                0 => (r >> 8) as u8,
                1 => 0xE3, // a kana lead byte
                2 => b'0' + (r >> 8) as u8 % 10,
                3 => (r >> 8) as u8 % 0x20, // control characters
                _ => b'a' + (r >> 8) as u8 % 26,
            }
        }
    }
}
