//! CPU feature discovery for startup-time kernel resolution (DESIGN 3.2).
//!
//! This module deliberately reports a *set* of available instructions rather
//! than a linear "widest tier".  AVX-512 extensions are not a total order, so
//! the only consumer of [`CpuFeatures`] is the startup resolver that decides
//! which concrete kernel is safe to publish.  Width-normalization hot paths
//! receive that already-selected kernel and never inspect these bits.

use core::fmt;

/// Runtime CPU capabilities observed during process startup.
///
/// The representation is intentionally private.  Consumers can ask whether a
/// capability set contains a requirement set, but cannot manufacture a value
/// in production code and accidentally publish an unsupported target-feature
/// function pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct CpuFeatures {
    bits: u8,
}

impl CpuFeatures {
    const AVX_BIT: u8 = 1 << 0;
    const SSSE3_BIT: u8 = 1 << 1;
    const AVX2_BIT: u8 = 1 << 2;
    const AVX512F_BIT: u8 = 1 << 3;
    const AVX512BW_BIT: u8 = 1 << 4;
    const AVX512VL_BIT: u8 = 1 << 5;

    /// No x86 vector capability.  This is a valid startup result only on
    /// non-x86-64 builds, where the scalar kernel is selected.
    pub const EMPTY: Self = Self { bits: 0 };

    /// The minimum feature set for the 128-bit compatibility scanner.
    ///
    /// `pshufb` is an SSSE3 instruction, so AVX alone is not sufficient for
    /// the scanner even though the shipped binary's broad baseline is AVX.
    pub const AVX_SSSE3: Self = Self {
        bits: Self::AVX_BIT | Self::SSSE3_BIT,
    };

    /// Requirements for the standard 256-bit scanner.
    pub const AVX2: Self = Self {
        bits: Self::AVX_SSSE3.bits | Self::AVX2_BIT,
    };

    /// Requirements for an AVX-512BW+VL width-scan strategy.
    ///
    /// F, BW, and VL are deliberately one requirement set. Diagnostics may
    /// benchmark a concrete AVX-512 strategy only with all three; the shipping
    /// resolver currently keeps AVX2 until stable end-to-end, cross-host
    /// evidence establishes an explicit admission policy.
    pub const AVX512_BW_VL: Self = Self {
        bits: Self::AVX2.bits | Self::AVX512F_BIT | Self::AVX512BW_BIT | Self::AVX512VL_BIT,
    };

    const fn from_detected(
        avx: bool,
        ssse3: bool,
        avx2: bool,
        avx512f: bool,
        avx512bw: bool,
        avx512vl: bool,
    ) -> Self {
        Self {
            bits: (if avx { Self::AVX_BIT } else { 0 })
                | (if ssse3 { Self::SSSE3_BIT } else { 0 })
                | (if avx2 { Self::AVX2_BIT } else { 0 })
                | (if avx512f { Self::AVX512F_BIT } else { 0 })
                | (if avx512bw { Self::AVX512BW_BIT } else { 0 })
                | (if avx512vl { Self::AVX512VL_BIT } else { 0 }),
        }
    }

    /// Whether every feature in `requirements` is present in this set.
    #[inline]
    pub const fn supports(self, requirements: Self) -> bool {
        self.bits & requirements.bits == requirements.bits
    }

    #[inline]
    pub const fn avx(self) -> bool {
        self.supports(Self {
            bits: Self::AVX_BIT,
        })
    }

    #[inline]
    pub const fn ssse3(self) -> bool {
        self.supports(Self {
            bits: Self::SSSE3_BIT,
        })
    }

    #[inline]
    pub const fn avx2(self) -> bool {
        self.supports(Self {
            bits: Self::AVX2_BIT,
        })
    }

    #[inline]
    pub const fn avx512f(self) -> bool {
        self.supports(Self {
            bits: Self::AVX512F_BIT,
        })
    }

    #[inline]
    pub const fn avx512bw(self) -> bool {
        self.supports(Self {
            bits: Self::AVX512BW_BIT,
        })
    }

    #[inline]
    pub const fn avx512vl(self) -> bool {
        self.supports(Self {
            bits: Self::AVX512VL_BIT,
        })
    }

    #[cfg(test)]
    pub(crate) const fn synthetic(
        avx: bool,
        ssse3: bool,
        avx2: bool,
        avx512f: bool,
        avx512bw: bool,
        avx512vl: bool,
    ) -> Self {
        Self::from_detected(avx, ssse3, avx2, avx512f, avx512bw, avx512vl)
    }
}

/// This CPU cannot safely run Sakura Input's width scanner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedCpu;

impl fmt::Display for UnsupportedCpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(
            "this CPU does not support the AVX + SSSE3 baseline which Sakura Input requires \
             (Intel Sandy Bridge / AMD Bulldozer, 2011 or later)",
        )
    }
}

impl std::error::Error for UnsupportedCpu {}

/// Reads the complete CPU feature set once for a startup resolver.
///
/// On x86-64, AVX and SSSE3 are both required because the narrowest vector
/// scanner uses `pshufb`.  On other architectures the core keeps its scalar
/// testability and reports no vector capabilities; the SIMD resolver selects
/// its scalar kernel there.
#[cfg(target_arch = "x86_64")]
pub(crate) fn detect_at_startup() -> Result<CpuFeatures, UnsupportedCpu> {
    let features = CpuFeatures::from_detected(
        is_x86_feature_detected!("avx"),
        is_x86_feature_detected!("ssse3"),
        is_x86_feature_detected!("avx2"),
        is_x86_feature_detected!("avx512f"),
        is_x86_feature_detected!("avx512bw"),
        is_x86_feature_detected!("avx512vl"),
    );
    if features.supports(CpuFeatures::AVX_SSSE3) {
        Ok(features)
    } else {
        Err(UnsupportedCpu)
    }
}

#[cfg(not(target_arch = "x86_64"))]
pub(crate) fn detect_at_startup() -> Result<CpuFeatures, UnsupportedCpu> {
    Ok(CpuFeatures::EMPTY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_compatibility_floor_requires_both_avx_and_ssse3() {
        assert!(
            !CpuFeatures::synthetic(true, false, false, false, false, false)
                .supports(CpuFeatures::AVX_SSSE3)
        );
        assert!(
            !CpuFeatures::synthetic(false, true, false, false, false, false)
                .supports(CpuFeatures::AVX_SSSE3)
        );
        assert!(
            CpuFeatures::synthetic(true, true, false, false, false, false)
                .supports(CpuFeatures::AVX_SSSE3)
        );
    }

    #[test]
    fn avx512_bw_vl_is_a_feature_set_not_a_tier() {
        let bw_without_vl = CpuFeatures::synthetic(true, true, true, true, true, false);
        let bw_with_vl = CpuFeatures::synthetic(true, true, true, true, true, true);
        assert!(!bw_without_vl.supports(CpuFeatures::AVX512_BW_VL));
        assert!(bw_with_vl.supports(CpuFeatures::AVX512_BW_VL));
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn a_machine_that_can_run_this_test_meets_the_compatibility_floor() {
        let features = detect_at_startup().expect("this test is built with the shipped baseline");
        assert!(features.avx());
        assert!(features.ssse3());
    }

    #[test]
    fn the_unsupported_message_names_every_baseline_requirement() {
        let text = UnsupportedCpu.to_string();
        assert!(text.contains("AVX"), "unhelpful message: {text}");
        assert!(text.contains("SSSE3"), "unhelpful message: {text}");
    }
}
