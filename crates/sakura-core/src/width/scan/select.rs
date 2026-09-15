//! Startup kernel selection.
//!
//! The immutable strategy records, the startup resolver that picks one of
//! them after CPU detection, and the single atomic pointer the hot path
//! loads. Nothing here runs per scan except [`active_width_scan`].

use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};
use std::sync::OnceLock;

use crate::cpu::{self, CpuFeatures, UnsupportedCpu};

#[cfg(target_arch = "x86_64")]
use super::kernels_x86::{
    scan_avx2, scan_avx512_bw_vl_from_128, scan_avx512_bw_vl_from_256, scan_avx512_bw_vl_from_64,
    scan_avx_ssse3_128,
};
use super::lut::scan_scalar;
#[cfg(target_arch = "x86_64")]
use super::lut::MIN_VECTOR_BYTES;
use super::{
    Avx512ZmmThreshold, KernelMetadata, KernelSet, ScanStrategy, WidthScanStrategy,
    WidthScanStrategyId,
};

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
pub(super) fn resolve_kernel_set(
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
pub(super) fn resolve_kernel_set(
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
pub(super) fn active_width_scan() -> &'static WidthScanStrategy {
    // SAFETY: initialized to `SCALAR_WIDTH_SCAN` and subsequently written only
    // with addresses of the immutable static strategy records above.
    unsafe { &*ACTIVE_WIDTH_SCAN.load(Ordering::Acquire) }
}
