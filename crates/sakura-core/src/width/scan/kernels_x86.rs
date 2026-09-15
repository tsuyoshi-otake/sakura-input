//! The x86-64 vector kernels.
//!
//! Each function carries its complete `#[target_feature]` contract, is
//! differential-tested against [`scan_scalar`], and is checked for
//! instruction shape by `ci/check-simd-assembly.ps1`. Only `select` publishes
//! pointers to them, and only after startup detected those features.

use super::lut::{scan_scalar, BIT_LUT};
#[cfg(test)]
use super::testing::note_avx_ssse3_xmm_call;
use super::{Avx512ZmmThreshold, Lut};

/// 16 bytes at a time — the x86-64 floor (DESIGN 3.2).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx,ssse3")]
pub(super) unsafe fn scan_avx_ssse3_128(src: &[u8], lut: &Lut) -> usize {
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
pub(super) unsafe fn scan_avx2(src: &[u8], lut: &Lut) -> usize {
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
pub(super) unsafe fn scan_avx512_bw_vl_from_64(src: &[u8], lut: &Lut) -> usize {
    // SAFETY: this wrapper carries the same complete target-feature contract
    // as the shared body, and only supplies one of its fixed thresholds.
    unsafe { scan_avx512_bw_vl_hybrid(src, lut, Avx512ZmmThreshold::From64.bytes()) }
}

/// AVX-512BW+VL strategy that reserves its ZMM body for 128-byte and longer
/// inputs.  It is selected only if startup measurements reject the 64-byte
/// takeover but accept this narrower owned range.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,avx512f,avx512bw,avx512vl,ssse3")]
pub(super) unsafe fn scan_avx512_bw_vl_from_128(src: &[u8], lut: &Lut) -> usize {
    // SAFETY: see `scan_avx512_bw_vl_from_64`.
    unsafe { scan_avx512_bw_vl_hybrid(src, lut, Avx512ZmmThreshold::From128.bytes()) }
}

/// AVX-512BW+VL strategy that reserves its ZMM body for 256-byte and longer
/// inputs.  This is the conservative final candidate when shorter long runs do
/// not clear the AVX2-relative gate on a particular processor.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,avx512f,avx512bw,avx512vl,ssse3")]
pub(super) unsafe fn scan_avx512_bw_vl_from_256(src: &[u8], lut: &Lut) -> usize {
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
