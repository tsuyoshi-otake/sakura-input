//! Test-only instrumentation, never compiled into a shipping build: kernel
//! call counters, AVX-512 path accounting for diagnostics, and the
//! direct-kernel AVX-512 calibration used by the ignored benchmark.

use super::lut::{admits, scan_scalar, MIN_VECTOR_BYTES};
use super::Lut;
#[cfg(target_arch = "x86_64")]
use super::{
    kernels_x86::{scan_avx2, scan_avx512_bw_vl_from_64},
    lut::passthrough_lut,
    Avx512ZmmThreshold, ScanStrategy,
};
#[cfg(target_arch = "x86_64")]
use crate::cpu::CpuFeatures;

#[cfg(test)]
std::thread_local! {
    pub(super) static SELECTED_WIDTH_SCAN_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static AVX_SSSE3_XMM_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Test-only evidence of which part of the AVX-512 hybrid a given scan uses.
///
/// This is derived outside the target-feature function, rather than incremented
/// inside its loops. Consequently, a release test benchmark measures the same
/// instruction sequence and register allocation as the shipped scanner.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Avx512PathCounts {
    pub(super) avx2_ymm_blocks: usize,
    pub(super) avx2_xmm_blocks: usize,
    pub(super) zmm_blocks: usize,
    pub(super) vl_ymm_blocks: usize,
    pub(super) vl_xmm_blocks: usize,
}

#[cfg(test)]
pub(super) fn note_selected_width_scan_call() {
    SELECTED_WIDTH_SCAN_CALLS.with(|calls| calls.set(calls.get() + 1));
}

#[cfg(test)]
pub(super) fn note_avx_ssse3_xmm_call() {
    AVX_SSSE3_XMM_CALLS.with(|calls| calls.set(calls.get() + 1));
}

#[cfg(test)]
pub(super) fn avx512_path_counts_for_scan(
    src: &[u8],
    lut: &Lut,
    zmm_min_bytes: usize,
) -> Avx512PathCounts {
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
pub(super) fn format_avx512_path(counts: Avx512PathCounts) -> String {
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
pub(super) fn avx512_path_counts_for_normalizer_runs(
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
pub(super) fn avx512_zmm_threshold_for_diagnostic(
    features: CpuFeatures,
) -> Option<Avx512ZmmThreshold> {
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
