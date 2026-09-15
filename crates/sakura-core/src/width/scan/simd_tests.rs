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
    let mut all: Vec<(&'static str, ScanStrategy)> = vec![("scalar", scan_scalar as ScanStrategy)];
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
        0, 1, 7, 8, 15, 16, 17, 31, 32, 33, 47, 48, 63, 64, 65, 95, 96, 127, 128, 129, 255, 256,
        257, 512,
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
    let (candidate_name, candidate_threshold, candidate) = avx512_candidate_for_benchmark(selected);
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
        "bytes", "candidate path", "avx2 p50", "avx512 p50", "ratio p10", "ratio p50", "ratio p90"
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
            let (base, candidate_time) =
                unsafe { nanos_interleaved_pair(baseline, candidate, src.as_slice(), lut, sample) };
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
    use sakura_values::{FixedStr, Mode};
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
        let wanted = (TARGET_SIDE_SAMPLE_NS / per_call_ns.max(f64::MIN_POSITIVE)).ceil() as usize;
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
                baseline_total_ns += measure_checked(baseline, chunk_rounds) * chunk_rounds as f64;
                candidate_total_ns +=
                    measure_checked(candidate, chunk_rounds) * chunk_rounds as f64;
            } else {
                candidate_total_ns +=
                    measure_checked(candidate, chunk_rounds) * chunk_rounds as f64;
                baseline_total_ns += measure_checked(baseline, chunk_rounds) * chunk_rounds as f64;
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
        punctuation: PunctuationStyle::KUTEN_TOUTEN,
        brackets: crate::width::BracketStyle::default(),
    };
    let baseline = scan_avx2 as ScanStrategy;
    let selected = startup().expect("the benchmark requires the compatibility floor");
    let (candidate_name, candidate_threshold, candidate) = avx512_candidate_for_benchmark(selected);

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
