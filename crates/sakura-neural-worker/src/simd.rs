//! One-time SIMD dispatch for bounded neural-worker preprocessing.
//!
//! `is_x86_feature_detected!` checks CPUID and the OS-enabled extended register
//! state required by AVX-family instructions (including OSXSAVE/XGETBV).  Keep
//! it here, during construction only: request processing calls the selected
//! function pointer and never re-runs feature detection.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Tier {
    Scalar = 0,
    AvxSsse3 = 1,
    Avx2Fma = 2,
    Avx512 = 3,
}

impl Tier {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Scalar => "scalar",
            Self::AvxSsse3 => "avx-ssse3-128",
            Self::Avx2Fma => "avx2-fma",
            Self::Avx512 => "avx512",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogitSummary {
    pub max: f32,
    pub log_sum_exp: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    AvxSsse3Unavailable,
    ForcedTierUnavailable,
    Avx512Unsupported,
    UnknownTier,
    EmptyLogits,
    TooManyLogits,
    NonFiniteLogit,
    ParityMismatch,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AvxSsse3Unavailable => "AVX and SSSE3 are required",
            Self::ForcedTierUnavailable => "forced SIMD tier is unavailable",
            Self::Avx512Unsupported => "AVX-512 is not enabled by this worker build",
            Self::UnknownTier => "unknown SIMD tier",
            Self::EmptyLogits => "logit input is empty",
            Self::TooManyLogits => "logit input exceeds the worker bound",
            Self::NonFiniteLogit => "logit input contains a non-finite value",
            Self::ParityMismatch => "SIMD preprocessing parity check failed",
        })
    }
}

impl std::error::Error for Error {}

pub const MAX_LOGITS: usize = 131_072;
type SummaryFn = unsafe fn(&[f32]) -> Result<LogitSummary, Error>;

/// Immutable after startup.  It can be shared by all requests because its
/// function pointer is selected once and the helper has no mutable state.
#[derive(Debug)]
pub struct Dispatch {
    tier: Tier,
    summarize: SummaryFn,
}

impl Dispatch {
    /// Selects the production tier once. AVX-512 is detected but deliberately
    /// capped at AVX2 until admission has measured it on supported hardware.
    pub fn startup() -> Result<Self, Error> {
        let features = Features::detect();
        if !features.avx_ssse3() {
            return Err(Error::AvxSsse3Unavailable);
        }
        Ok(Self::for_available_tier(if features.avx2_fma() {
            Tier::Avx2Fma
        } else {
            Tier::AvxSsse3
        }))
    }

    /// Selects a tier only for `--self-test --force-tier`. Unsupported CPU
    /// tiers are rejected before any target-feature function can execute.
    pub fn force_for_self_test(name: &str) -> Result<Self, Error> {
        let tier = match name {
            "scalar" => Tier::Scalar,
            "avx" => Tier::AvxSsse3,
            "avx2" => Tier::Avx2Fma,
            "avx512" => Tier::Avx512,
            _ => return Err(Error::UnknownTier),
        };
        let features = Features::detect();
        match tier {
            Tier::Scalar => Ok(Self::for_available_tier(Tier::Scalar)),
            Tier::AvxSsse3 if features.avx_ssse3() => Ok(Self::for_available_tier(tier)),
            Tier::Avx2Fma if features.avx2_fma() => Ok(Self::for_available_tier(tier)),
            // Detect AVX-512F/BW/VL so a force request has a precise terminal
            // outcome, but never execute it: there is no admitted AVX-512
            // kernel table in this build.
            Tier::Avx512 if features.avx512_f_bw_vl() => Err(Error::Avx512Unsupported),
            Tier::Avx512 => Err(Error::ForcedTierUnavailable),
            _ => Err(Error::ForcedTierUnavailable),
        }
    }

    pub fn tier(&self) -> Tier {
        self.tier
    }

    /// Bounded stable log-sum-exp preprocessing for masked-token PLL scoring.
    pub fn summarize(&self, logits: &[f32]) -> Result<LogitSummary, Error> {
        // SAFETY: construction chooses this pointer only after the corresponding
        // OS-aware CPU feature check. Dispatch is immutable, and the scalar
        // pointer is always valid. Request processing does not detect features.
        unsafe { (self.summarize)(logits) }
    }

    fn for_available_tier(tier: Tier) -> Self {
        let summarize = match tier {
            Tier::Scalar => scalar_summary as SummaryFn,
            Tier::AvxSsse3 => avx_summary as SummaryFn,
            Tier::Avx2Fma => avx2_fma_summary as SummaryFn,
            Tier::Avx512 => unreachable!("AVX-512 has no executable table"),
        };
        Self { tier, summarize }
    }
}

#[derive(Debug, Clone, Copy)]
struct Features {
    avx: bool,
    ssse3: bool,
    avx2: bool,
    fma: bool,
    avx512f: bool,
    avx512bw: bool,
    avx512vl: bool,
}

impl Features {
    fn detect() -> Self {
        #[cfg(test)]
        DETECTION_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // The macro has the required x86 and non-x86 handling. Its AVX checks
        // include OSXSAVE/XGETBV state validation, unlike raw CPUID bit tests.
        Self {
            avx: std::is_x86_feature_detected!("avx"),
            ssse3: std::is_x86_feature_detected!("ssse3"),
            avx2: std::is_x86_feature_detected!("avx2"),
            fma: std::is_x86_feature_detected!("fma"),
            avx512f: std::is_x86_feature_detected!("avx512f"),
            avx512bw: std::is_x86_feature_detected!("avx512bw"),
            avx512vl: std::is_x86_feature_detected!("avx512vl"),
        }
    }

    fn avx_ssse3(self) -> bool {
        self.avx && self.ssse3
    }

    fn avx2_fma(self) -> bool {
        self.avx_ssse3() && self.avx2 && self.fma
    }

    fn avx512_f_bw_vl(self) -> bool {
        self.avx_ssse3() && self.avx512f && self.avx512bw && self.avx512vl
    }
}

unsafe fn scalar_summary(logits: &[f32]) -> Result<LogitSummary, Error> {
    summary_with_max(logits, scalar_max(logits)?)
}

#[target_feature(enable = "avx,ssse3")]
unsafe fn avx_summary(logits: &[f32]) -> Result<LogitSummary, Error> {
    // SAFETY: this target-feature function is reachable only through the AVX
    // dispatch selected after OS-aware feature detection.
    let max = unsafe { avx_finite_max(logits) }?;
    summary_with_max(logits, max)
}

#[target_feature(enable = "avx,avx2,fma,ssse3")]
unsafe fn avx2_fma_summary(logits: &[f32]) -> Result<LogitSummary, Error> {
    // SAFETY: the startup dispatch proves AVX, AVX2, FMA, and SSSE3 support
    // before installing this function pointer.
    let max = unsafe { avx_finite_max(logits) }?;
    // SAFETY: the same dispatch proof covers the AVX2/FMA helper.
    let shifted_sum = unsafe { avx2_fma_shifted_exp_sum(logits, max) }?;
    Ok(LogitSummary {
        max,
        log_sum_exp: max + shifted_sum.ln() as f32,
    })
}

fn scalar_max(logits: &[f32]) -> Result<f32, Error> {
    validate_length(logits)?;
    let mut max = f32::NEG_INFINITY;
    for &logit in logits {
        if !logit.is_finite() {
            return Err(Error::NonFiniteLogit);
        }
        max = max.max(logit);
    }
    Ok(max)
}

fn summary_with_max(logits: &[f32], max: f32) -> Result<LogitSummary, Error> {
    // The scalar pass makes the final f64 accumulation deterministic across
    // scalar and AVX baseline tiers. It is bounded by MAX_LOGITS.
    let mut shifted_sum = 0.0_f64;
    for &logit in logits {
        shifted_sum += f64::from(logit - max).exp();
    }
    if !shifted_sum.is_finite() || shifted_sum <= 0.0 {
        return Err(Error::NonFiniteLogit);
    }
    Ok(LogitSummary {
        max,
        log_sum_exp: max + shifted_sum.ln() as f32,
    })
}

fn validate_length(logits: &[f32]) -> Result<(), Error> {
    if logits.is_empty() {
        Err(Error::EmptyLogits)
    } else if logits.len() > MAX_LOGITS {
        Err(Error::TooManyLogits)
    } else {
        Ok(())
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
unsafe fn avx_finite_max(logits: &[f32]) -> Result<f32, Error> {
    use std::arch::x86_64::{
        _mm256_andnot_ps, _mm256_cmp_ps, _mm256_loadu_ps, _mm256_max_ps, _mm256_movemask_ps,
        _mm256_set1_ps, _mm256_storeu_ps, _CMP_LE_OQ,
    };

    validate_length(logits)?;
    // SAFETY: this helper is called only from an AVX target-feature function
    // installed after OS-aware AVX detection.
    let (sign, max_finite, mut lane_max) = unsafe {
        (
            _mm256_set1_ps(-0.0),
            _mm256_set1_ps(f32::MAX),
            _mm256_set1_ps(f32::NEG_INFINITY),
        )
    };
    let mut offset = 0;
    while offset + 8 <= logits.len() {
        // SAFETY: the loop condition proves eight f32 values are in-bounds;
        // loadu accepts any alignment and the target feature is on the caller.
        let (lanes, mask) = unsafe {
            let lanes = _mm256_loadu_ps(logits.as_ptr().add(offset));
            let absolute = _mm256_andnot_ps(sign, lanes);
            let finite = _mm256_cmp_ps(absolute, max_finite, _CMP_LE_OQ);
            (lanes, _mm256_movemask_ps(finite))
        };
        if mask != 0xff {
            return Err(Error::NonFiniteLogit);
        }
        // SAFETY: AVX availability is part of this helper's call contract.
        lane_max = unsafe { _mm256_max_ps(lane_max, lanes) };
        offset += 8;
    }

    let mut maxima = [f32::NEG_INFINITY; 8];
    // SAFETY: maxima has eight contiguous f32 elements for the unaligned store.
    unsafe { _mm256_storeu_ps(maxima.as_mut_ptr(), lane_max) };
    let mut max = maxima.into_iter().fold(f32::NEG_INFINITY, f32::max);
    for &logit in &logits[offset..] {
        if !logit.is_finite() {
            return Err(Error::NonFiniteLogit);
        }
        max = max.max(logit);
    }
    Ok(max)
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
unsafe fn avx_finite_max(logits: &[f32]) -> Result<f32, Error> {
    let _ = logits;
    Err(Error::AvxSsse3Unavailable)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
unsafe fn avx2_fma_shifted_exp_sum(logits: &[f32], max: f32) -> Result<f64, Error> {
    use std::arch::x86_64::{_mm256_fmadd_ps, _mm256_loadu_ps, _mm256_set1_ps, _mm256_storeu_ps};

    let mut shifted_sum = 0.0_f64;
    // SAFETY: this helper is called only from the AVX2/FMA target-feature
    // function installed after OS-aware feature detection.
    let (one, negative_max) = unsafe { (_mm256_set1_ps(1.0), _mm256_set1_ps(-max)) };
    let mut offset = 0;
    while offset + 8 <= logits.len() {
        // SAFETY: the loop condition proves eight f32 values are in-bounds;
        // AVX2/FMA availability was checked before this function pointer exists.
        let shifted = unsafe {
            let lanes = _mm256_loadu_ps(logits.as_ptr().add(offset));
            _mm256_fmadd_ps(lanes, one, negative_max)
        };
        let mut values = [0.0_f32; 8];
        // SAFETY: values has eight contiguous f32 elements for this store.
        unsafe { _mm256_storeu_ps(values.as_mut_ptr(), shifted) };
        for value in values {
            shifted_sum += f64::from(value).exp();
        }
        offset += 8;
    }
    for &logit in &logits[offset..] {
        shifted_sum += f64::from(logit - max).exp();
    }
    if !shifted_sum.is_finite() || shifted_sum <= 0.0 {
        return Err(Error::NonFiniteLogit);
    }
    Ok(shifted_sum)
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
unsafe fn avx2_fma_shifted_exp_sum(_logits: &[f32], _max: f32) -> Result<f64, Error> {
    Err(Error::ForcedTierUnavailable)
}

/// Exercises scalar and an admitted forced tier with normal and tail lengths.
pub fn self_test(dispatch: &Dispatch) -> Result<(), Error> {
    let scalar = Dispatch::for_available_tier(Tier::Scalar);
    for logits in [&[0.0_f32][..], &[2.0, -1.0, 3.5, 0.25, 9.0][..]] {
        let expected = scalar.summarize(logits)?;
        let actual = dispatch.summarize(logits)?;
        if (expected.max - actual.max).abs() > 1e-6
            || (expected.log_sum_exp - actual.log_sum_exp).abs() > 1e-5
        {
            return Err(Error::ParityMismatch);
        }
    }
    Ok(())
}

#[cfg(test)]
static DETECTION_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::Ordering, Mutex};

    static DETECTION_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn scalar() -> Dispatch {
        Dispatch::for_available_tier(Tier::Scalar)
    }

    #[test]
    fn scalar_handles_empty_nonfinite_and_bounds() {
        let dispatch = scalar();
        assert_eq!(dispatch.summarize(&[]), Err(Error::EmptyLogits));
        assert_eq!(dispatch.summarize(&[f32::NAN]), Err(Error::NonFiniteLogit));
        assert_eq!(
            dispatch.summarize(&[f32::INFINITY]),
            Err(Error::NonFiniteLogit)
        );
        assert_eq!(
            dispatch.summarize(&vec![0.0; MAX_LOGITS + 1]),
            Err(Error::TooManyLogits)
        );
    }

    #[test]
    fn scalar_and_supported_tiers_are_within_tolerance_for_tails() {
        let scalar = scalar();
        let logits = [3.0, -1.0, 0.25, 8.0, -4.0, 7.5, 2.0, 1.0, -0.5, 0.125];
        let expected = scalar.summarize(&logits).unwrap();
        for name in ["avx", "avx2"] {
            if let Ok(dispatch) = Dispatch::force_for_self_test(name) {
                let actual = dispatch.summarize(&logits).unwrap();
                assert!((actual.max - expected.max).abs() <= 1e-6);
                assert!((actual.log_sum_exp - expected.log_sum_exp).abs() <= 1e-5);
            }
        }
    }

    #[test]
    fn generated_finite_logits_preserve_scalar_parity_across_admitted_tiers() {
        let scalar = scalar();
        let tiers: Vec<_> = ["avx", "avx2"]
            .into_iter()
            .filter_map(|name| Dispatch::force_for_self_test(name).ok())
            .collect();
        let mut state = 0xd1b5_4a32_d192_ed03u64;
        for case in 0..256usize {
            let length = 1 + (case * 37 % 513);
            let mut logits = Vec::with_capacity(length);
            for _ in 0..length {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let unit = (state >> 40) as f32 / ((1u32 << 24) - 1) as f32;
                logits.push((unit - 0.5) * 64.0);
            }
            let expected = scalar.summarize(&logits).unwrap();
            for dispatch in &tiers {
                let actual = dispatch.summarize(&logits).unwrap();
                assert_eq!(actual.max, expected.max, "case={case}");
                assert!(
                    (actual.log_sum_exp - expected.log_sum_exp).abs() <= 5e-5,
                    "case={case}, tier={:?}, expected={}, actual={}",
                    dispatch.tier(),
                    expected.log_sum_exp,
                    actual.log_sum_exp
                );
            }
        }
    }

    #[test]
    fn deliberately_unaligned_slices_preserve_simd_parity() {
        let scalar = scalar();
        for length in [1usize, 7, 8, 9, 31, 32, 33, 127, 128, 129] {
            let mut storage = vec![0.0f32; length + 8];
            let offset = (0..8)
                .find(|offset| {
                    // The test deliberately avoids a 32-byte boundary. The
                    // production kernel uses loadu/storeu and must accept it.
                    !(storage.as_ptr().wrapping_add(*offset) as usize).is_multiple_of(32)
                })
                .unwrap();
            let logits = &mut storage[offset..offset + length];
            for (index, value) in logits.iter_mut().enumerate() {
                *value = ((index * 17 + length * 3) % 97) as f32 / 3.0 - 16.0;
            }
            assert!(!(logits.as_ptr() as usize).is_multiple_of(32));
            let expected = scalar.summarize(logits).unwrap();
            for name in ["avx", "avx2"] {
                if let Ok(dispatch) = Dispatch::force_for_self_test(name) {
                    let actual = dispatch.summarize(logits).unwrap();
                    assert_eq!(actual.max, expected.max, "length={length}, tier={name}");
                    assert!(
                        (actual.log_sum_exp - expected.log_sum_exp).abs() <= 5e-5,
                        "length={length}, tier={name}"
                    );
                }
            }
        }
    }

    #[test]
    fn forcing_avx512_is_terminal_and_never_executes_it() {
        assert!(matches!(
            Dispatch::force_for_self_test("avx512"),
            Err(Error::ForcedTierUnavailable | Error::Avx512Unsupported)
        ));
        assert_eq!(
            Dispatch::force_for_self_test("unknown").unwrap_err(),
            Error::UnknownTier
        );
    }

    #[test]
    fn production_never_admits_avx512() {
        if let Ok(dispatch) = Dispatch::startup() {
            assert_ne!(dispatch.tier(), Tier::Avx512);
        }
    }

    #[test]
    fn request_path_does_not_re_detect_features() {
        let _guard = DETECTION_TEST_LOCK.lock().unwrap();
        let before = DETECTION_COUNT.load(Ordering::Relaxed);
        let dispatch = Dispatch::force_for_self_test("scalar").unwrap();
        let after_construct = DETECTION_COUNT.load(Ordering::Relaxed);
        assert_eq!(after_construct, before + 1);
        dispatch.summarize(&[0.0, 1.0, -1.0]).unwrap();
        assert_eq!(DETECTION_COUNT.load(Ordering::Relaxed), after_construct);
    }
}
