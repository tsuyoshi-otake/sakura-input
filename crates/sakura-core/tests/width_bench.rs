//! What the run scanner is worth, measured rather than argued.
//!
//! `Normalizer::normalize_into` used to be `src.chars()` mapped through
//! `normalize_char`; it now looks for runs of bytes the policy leaves alone
//! and copies each run in one move (DESIGN 5.6). That is only an improvement
//! if the runs are long enough to pay for finding them, and the honest answer
//! depends entirely on what the text looks like — so this measures four
//! corpora that bracket the real traffic, under both a policy that lets ASCII
//! through and one that rewrites every byte of it.
//!
//! Ignored by default, and deliberately not an assertion. A timing threshold
//! on a shared CI runner is a coin flip that eventually gets muted, and a
//! muted test is worse than no test. This exists to be run on purpose:
//!
//! ```text
//! cargo test -p sakura-core --release -- --ignored --nocapture
//! ```
//!
//! Debug numbers are meaningless here — without optimization the vector
//! kernels are wrapped in unelided bounds checks and the scalar loop wins.

use std::hint::black_box;
use std::time::{Duration, Instant};

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{_mm_lfence, _rdtsc};

use sakura_core::text::TextSink;
use sakura_core::width::{BracketStyle, Normalizer, PunctuationStyle, Width, WidthPolicy};
use sakura_proto::{FixedStr, Mode};

/// Sized for the longest corpus at its worst case: every ASCII byte becoming
/// a three-byte full-width character.
type Sink = FixedStr<1024>;

/// Enough rounds that a corpus of a few dozen bytes still lands well clear of
/// the clock's resolution, and few enough that the whole test finishes in a
/// couple of seconds.
const ROUNDS: usize = 50_000;

/// How many times each measurement is repeated before the fastest is kept.
const REPEATS: usize = 7;

/// The four shapes of text the normalizer actually sees. The first is the
/// one that matters most and flatters SIMD least: a single keystroke is far
/// below the vector threshold, so it measures the cost of *deciding* not to
/// vectorize.
const CORPORA: &[(&str, &str)] = &[
    ("one keystroke", "k"),
    (
        "ascii command",
        "docker compose up -d --build --remove-orphans",
    ),
    (
        "japanese prose",
        "この変換候補は直前に確定した内容を考慮して並べ替えられます。",
    ),
    (
        "mixed",
        "Docker のビルドキャッシュは ~/.cache/docker に置く、CI でも同じ。",
    ),
];

/// The previous implementation, kept here as the thing to beat. If this ever
/// stops being observably equivalent to `normalize_into`, `width.rs`'s own
/// tests fail long before this one prints anything.
///
/// It writes through [`TextSink`] rather than through `FixedStr`'s inherent
/// `push`, because that is what `normalize_into` is obliged to do — comparing
/// against a version that skips the trait would be measuring the trait, not
/// the run scanner.
fn per_char(normalizer: &Normalizer, src: &str, mode: Mode, dst: &mut impl TextSink) {
    for c in src.chars() {
        dst.push(normalizer.normalize_char(c, mode))
            .expect("the sink is sized for the corpus");
    }
}

/// The best of [`REPEATS`] runs, in nanoseconds per call.
///
/// The *minimum*, not the mean: everything that makes a run slower — a
/// scheduler preemption, a frequency dip, a neighbouring process evicting the
/// cache — is additive noise on top of a fixed cost, so the fastest run is
/// the closest estimate of what the code actually costs. Taking the mean
/// instead produced 53 ns and 100 ns for the same unchanged loop on two
/// consecutive runs of this file, which is not a measurement.
fn nanos_each(mut body: impl FnMut()) -> f64 {
    for _ in 0..ROUNDS / 10 {
        body();
    }
    let mut best = Duration::MAX;
    for _ in 0..REPEATS {
        let start = Instant::now();
        for _ in 0..ROUNDS {
            body();
        }
        best = best.min(start.elapsed());
    }
    best.as_secs_f64() * 1e9 / ROUNDS as f64
}

/// The best of [`REPEATS`] serialized-TSC measurements, in CPU cycles per
/// call. The project ships only on x86-64, so this is the most direct A/B
/// signal for a small branch-and-loop change: unlike wall time it is not
/// distorted by core-frequency changes between the baseline and candidate.
#[cfg(target_arch = "x86_64")]
fn cycles_each(mut body: impl FnMut()) -> f64 {
    for _ in 0..ROUNDS / 10 {
        body();
    }
    let mut best = u64::MAX;
    for _ in 0..REPEATS {
        // SAFETY: LFENCE serializes execution around RDTSC on the supported
        // x86-64 target; neither intrinsic reads or writes memory.
        let start = unsafe {
            _mm_lfence();
            _rdtsc()
        };
        for _ in 0..ROUNDS {
            body();
        }
        // SAFETY: same serialized TSC read as above, after the measured body
        // has completed.
        let end = unsafe {
            _mm_lfence();
            _rdtsc()
        };
        best = best.min(end.wrapping_sub(start));
    }
    best as f64 / ROUNDS as f64
}

#[cfg(target_arch = "x86_64")]
#[test]
#[ignore = "timing, not a threshold: run with --release --ignored --nocapture and read it"]
fn issue_110_width_paths_in_cpu_cycles() {
    let default = Normalizer::default();
    let all_full = Normalizer {
        width: WidthPolicy {
            alnum: Width::Full,
            number: Width::Full,
            symbol: Width::Full,
        },
        punctuation: PunctuationStyle::KUTEN_TOUTEN,
        brackets: BracketStyle::default(),
    };
    let follow_mode = Normalizer {
        width: WidthPolicy {
            alnum: Width::FollowMode,
            number: Width::FollowMode,
            symbol: Width::FollowMode,
        },
        punctuation: PunctuationStyle::KUTEN_TOUTEN,
        brackets: BracketStyle::default(),
    };
    let cases = [
        ("default one-key", &default, Mode::Hiragana, "k"),
        (
            "default ASCII",
            &default,
            Mode::Hiragana,
            "docker compose up -d --build --remove-orphans",
        ),
        (
            "default Japanese",
            &default,
            Mode::Hiragana,
            "この変換候補は直前に確定した内容を考慮して並べ替えられます。",
        ),
        (
            "default mixed",
            &default,
            Mode::Hiragana,
            "Docker のビルドキャッシュは ~/.cache/docker に置く、CI でも同じ。",
        ),
        (
            "all-full ASCII",
            &all_full,
            Mode::Hiragana,
            "docker compose up -d --build --remove-orphans",
        ),
        (
            "all-full identifier",
            &all_full,
            Mode::Hiragana,
            "issue_110_width_normalizer_fast_path_identifier_2026",
        ),
        (
            "follow/full-alnum",
            &follow_mode,
            Mode::FullAlnum,
            "issue_110_width_normalizer_fast_path_identifier_2026",
        ),
        (
            "space/control-heavy",
            &all_full,
            Mode::Hiragana,
            concat!(
                " \t\n\r\0  \u{1}\u{7f} \t\n\r\0  \u{1}\u{7f} ",
                " \t\n\r\0  \u{1}\u{7f} \t\n\r\0  \u{1}\u{7f} ",
                " \t\n\r\0  \u{1}\u{7f} \t\n\r\0  \u{1}\u{7f} ",
            ),
        ),
    ];

    println!("\n{:<22} {:>5} {:>14}", "case", "bytes", "cycles/call");
    for (name, normalizer, mode, src) in cases {
        let cycles = cycles_each(|| {
            let mut dst = Sink::new();
            normalizer
                .normalize_into(black_box(src), mode, &mut dst)
                .expect("the sink is sized for the corpus");
            black_box(dst.len());
        });
        println!("{name:<22} {:>5} {cycles:>14.2}", src.len());
    }
}

#[test]
#[ignore = "timing, not a threshold: run with --release --ignored --nocapture and read it"]
fn what_the_run_scanner_costs_and_saves() {
    let half = Normalizer::default();
    let full = Normalizer {
        width: WidthPolicy {
            alnum: Width::Full,
            number: Width::Full,
            symbol: Width::Full,
        },
        punctuation: PunctuationStyle::KUTEN_TOUTEN,
        brackets: BracketStyle::default(),
    };
    // `Width::Full` ignores the mode, so one mode covers both policies and
    // the two rows differ only in the thing being measured.
    let mode = Mode::Hiragana;

    println!();
    let kernel = sakura_core::simd::startup()
        .expect("the benchmark requires the AVX + SSSE3 compatibility floor")
        .width_scan()
        .metadata();
    println!(
        "resolved width-scan strategy: {} ({}-byte main block; scalar below {} bytes), {ROUNDS} rounds",
        kernel.name, kernel.block_bytes, kernel.minimum_bytes,
    );
    println!(
        "This names the selected strategy, not a claim that every corpus executes its main vector block."
    );
    println!(
        "\n{:<16} {:<12} {:>5} {:>12} {:>12} {:>9}",
        "corpus", "policy", "bytes", "per-char ns", "runs ns", "speedup",
    );

    for (corpus, src) in CORPORA {
        for (policy, normalizer) in [("half (pass)", &half), ("full (rewrite)", &full)] {
            let old_ns = nanos_each(|| {
                let mut dst = Sink::new();
                per_char(normalizer, black_box(src), mode, &mut dst);
                black_box(dst.len());
            });
            let new_ns = nanos_each(|| {
                let mut dst = Sink::new();
                normalizer
                    .normalize_into(black_box(src), mode, &mut dst)
                    .expect("the sink is sized for the corpus");
                black_box(dst.len());
            });

            println!(
                "{:<16} {:<12} {:>5} {:>12.1} {:>12.1} {:>8.2}x",
                corpus,
                policy,
                src.len(),
                old_ns,
                new_ns,
                old_ns / new_ns,
            );
        }
    }

    println!(
        "\nRead this as: the run path wins where ASCII survives the policy, \
         and must not lose badly where it does not.",
    );
}
