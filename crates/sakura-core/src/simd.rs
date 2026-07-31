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

use std::sync::OnceLock;

use crate::cpu::{self, Tier};

/// A passthrough set, indexed by low nibble: `lut[lo]` has bit `hi` set when
/// the byte `hi << 4 | lo` is left unchanged by the policy the table was
/// built for.
pub type Lut = [u8; 16];

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
/// channel, space and ASCII punctuation the symbol channel, and control
/// characters are outside the policy entirely. Japanese punctuation cannot
/// appear here — none of the four code points it owns is ASCII.
///
/// Bytes at or above `0x80` are not single-byte characters at all; callers
/// must reject them before consulting this, which the tables do structurally
/// by leaving those high nibbles unset.
const fn passes_through(b: u8, full_alpha: bool, full_digit: bool, full_symbol: bool) -> bool {
    match b {
        b'A'..=b'Z' | b'a'..=b'z' => !full_alpha,
        b'0'..=b'9' => !full_digit,
        // Space plus every ASCII punctuation range, i.e. all of 0x20..=0x7E
        // that is not a letter or a digit.
        0x20..=0x2F | 0x3A..=0x40 | 0x5B..=0x60 | 0x7B..=0x7E => !full_symbol,
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
    // SAFETY: `resolve` chooses from `cpu::tier()`, which reports only
    // features this CPU was measured to have, and the choice is cached
    // rather than recomputed, so the kernel cannot drift away from the
    // machine it was chosen for.
    unsafe { kernel()(src, lut) }
}

/// A scanner. `unsafe` because the vector implementations require the caller
/// to have established that this machine has the instructions they use;
/// [`kernel`] is the only intended source of one.
type Kernel = unsafe fn(&[u8], &Lut) -> usize;

/// The kernel for this machine, resolved once.
#[inline]
fn kernel() -> Kernel {
    static KERNEL: OnceLock<Kernel> = OnceLock::new();
    *KERNEL.get_or_init(resolve)
}

#[cfg(target_arch = "x86_64")]
fn resolve() -> Kernel {
    match cpu::tier() {
        Tier::Avx512 => scan_avx512 as Kernel,
        Tier::Avx2 => scan_avx2 as Kernel,
        Tier::Avx => scan_avx as Kernel,
        Tier::Scalar => scan_scalar as Kernel,
    }
}

/// Off x86-64 there is one kernel and no choice to make. `cpu` is still
/// consulted so the tier is resolved (and logged) uniformly on every target.
#[cfg(not(target_arch = "x86_64"))]
fn resolve() -> Kernel {
    debug_assert_eq!(cpu::tier(), Tier::Scalar);
    scan_scalar as Kernel
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
unsafe fn scan_avx(src: &[u8], lut: &Lut) -> usize {
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

/// 32 bytes at a time, finishing through the narrower kernels so the tail is
/// never more than 15 scalar bytes.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn scan_avx2(src: &[u8], lut: &Lut) -> usize {
    use core::arch::x86_64::{
        __m128i, __m256i, _mm256_and_si256, _mm256_broadcastsi128_si256, _mm256_cmpeq_epi8,
        _mm256_loadu_si256, _mm256_movemask_epi8, _mm256_set1_epi8, _mm256_setzero_si256,
        _mm256_shuffle_epi8, _mm256_srli_epi16, _mm_loadu_si128,
    };

    const LANES: usize = 32;
    let mut at = 0usize;
    // SAFETY: as `scan_avx`, with 32-byte loads bounded by the same loop
    // condition. `_mm256_shuffle_epi8` works within each 128-bit half, which
    // is why both tables are broadcast to both halves rather than loaded
    // once into the low one.
    unsafe {
        let table = _mm256_broadcastsi128_si256(_mm_loadu_si128(lut.as_ptr().cast::<__m128i>()));
        let bits = _mm256_broadcastsi128_si256(_mm_loadu_si128(BIT_LUT.as_ptr().cast::<__m128i>()));
        let low = _mm256_set1_epi8(0x0f);
        let zero = _mm256_setzero_si256();
        while at + LANES <= src.len() {
            let v = _mm256_loadu_si256(src.as_ptr().add(at).cast::<__m256i>());
            let lo = _mm256_and_si256(v, low);
            let hi = _mm256_and_si256(_mm256_srli_epi16::<4>(v), low);
            let hit = _mm256_and_si256(
                _mm256_shuffle_epi8(table, lo),
                _mm256_shuffle_epi8(bits, hi),
            );
            let stop = _mm256_movemask_epi8(_mm256_cmpeq_epi8(hit, zero)) as u32;
            if stop != 0 {
                return at + stop.trailing_zeros() as usize;
            }
            at += LANES;
        }
        at + scan_avx(&src[at..], lut)
    }
}

/// 64 bytes at a time.
///
/// AVX-512BW rather than AVX-512F: the byte-granularity shuffle and the
/// per-byte mask test are BW instructions, and a CPU with F alone would run
/// none of them.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f,avx512bw")]
unsafe fn scan_avx512(src: &[u8], lut: &Lut) -> usize {
    use core::arch::x86_64::{
        __m128i, _mm512_and_si512, _mm512_broadcast_i32x4, _mm512_loadu_si512, _mm512_set1_epi8,
        _mm512_shuffle_epi8, _mm512_srli_epi16, _mm512_testn_epi8_mask, _mm_loadu_si128,
    };

    const LANES: usize = 64;
    let mut at = 0usize;
    // SAFETY: as `scan_avx2`, with 64-byte loads bounded by the same loop
    // condition. `_mm512_shuffle_epi8` also works within 128-bit lanes, so
    // the tables are broadcast to all four.
    unsafe {
        let table = _mm512_broadcast_i32x4(_mm_loadu_si128(lut.as_ptr().cast::<__m128i>()));
        let bits = _mm512_broadcast_i32x4(_mm_loadu_si128(BIT_LUT.as_ptr().cast::<__m128i>()));
        let low = _mm512_set1_epi8(0x0f);
        while at + LANES <= src.len() {
            let v = _mm512_loadu_si512(src.as_ptr().add(at).cast());
            let lo = _mm512_and_si512(v, low);
            let hi = _mm512_and_si512(_mm512_srli_epi16::<4>(v), low);
            let hit = _mm512_and_si512(
                _mm512_shuffle_epi8(table, lo),
                _mm512_shuffle_epi8(bits, hi),
            );
            // One instruction where the narrower kernels need a compare and
            // a movemask: the mask registers are the point of AVX-512.
            let stop = _mm512_testn_epi8_mask(hit, hit);
            if stop != 0 {
                return at + stop.trailing_zeros() as usize;
            }
            at += LANES;
        }
        at + scan_avx2(&src[at..], lut)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A byte no policy ever changes, for building runs of known length.
    const FILLER: u8 = 0x01;

    /// Every kernel this machine can actually run, named for assertion
    /// messages. The scalar reference is always first.
    fn kernels() -> Vec<(&'static str, Kernel)> {
        #[allow(unused_mut)]
        let mut all: Vec<(&'static str, Kernel)> = vec![("scalar", scan_scalar as Kernel)];
        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx") && is_x86_feature_detected!("ssse3") {
                all.push(("avx", scan_avx as Kernel));
            }
            if is_x86_feature_detected!("avx2") {
                all.push(("avx2", scan_avx2 as Kernel));
            }
            if is_x86_feature_detected!("avx512f") && is_x86_feature_detected!("avx512bw") {
                all.push(("avx512", scan_avx512 as Kernel));
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
        println!(
            "kernels under test: {names:?} (tier {})",
            cpu::tier().name()
        );
        assert!(names.contains(&"scalar"));
        #[cfg(target_arch = "x86_64")]
        assert!(
            names.contains(&"avx"),
            "this binary is built with +avx, so AVX cannot be missing here"
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
        assert!(!admits(symbol_only, b' '), "space is a symbol");

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
