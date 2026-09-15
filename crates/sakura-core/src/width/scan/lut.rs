//! The passthrough tables and the scalar reference scan.
//!
//! Everything that defines *which* bytes pass through lives here: the
//! semantic predicate, the eight compile-time nibble tables built from it,
//! the byte test [`admits`], and [`scan_scalar`], the definition of the right
//! answer that every vector kernel is differential-tested against.

use super::Lut;

/// `1 << hi` for each high nibble an ASCII byte can have, and zero for the
/// eight that only a continuation or lead byte can have. Those zeros are
/// what make non-ASCII end a run without a second comparison.
pub(super) const BIT_LUT: Lut = [1, 2, 4, 8, 16, 32, 64, 128, 0, 0, 0, 0, 0, 0, 0, 0];

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
pub(super) const fn passes_through(
    b: u8,
    full_alpha: bool,
    full_digit: bool,
    full_symbol: bool,
) -> bool {
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

/// The reference implementation: the definition of the right answer, and the
/// tail of every vector kernel.
pub(super) fn scan_scalar(src: &[u8], lut: &Lut) -> usize {
    src.iter()
        .position(|&b| !admits(lut, b))
        .unwrap_or(src.len())
}
