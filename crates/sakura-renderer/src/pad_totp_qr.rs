//! Small, fixed-profile QR encoder for the bounded Pad enrollment URI.
//!
//! Encodes UTF-8 bytes in QR byte mode, Version 8, error correction M. The
//! profile carries at most 152 bytes and produces a 49×49 module matrix.

use zeroize::{Zeroize, Zeroizing};

const VERSION: usize = 8;
const SIZE: usize = 17 + VERSION * 4;
const DATA_CODEWORDS: usize = 154;
const ECC_PER_BLOCK: usize = 22;
const MAX_BYTES: usize = 152;

pub(super) fn encode(text: &str) -> Option<Zeroizing<Vec<u8>>> {
    let bytes = text.as_bytes();
    if bytes.len() > MAX_BYTES {
        return None;
    }

    let mut bits = Zeroizing::new(Vec::with_capacity(DATA_CODEWORDS * 8));
    append_bits(&mut bits, 0b0100, 4);
    append_bits(&mut bits, bytes.len(), 8);
    for &byte in bytes {
        append_bits(&mut bits, byte as usize, 8);
    }
    let capacity = DATA_CODEWORDS * 8;
    for _ in 0..(capacity - bits.len()).min(4) {
        bits.push(false);
    }
    while bits.len() % 8 != 0 {
        bits.push(false);
    }
    let mut data = Zeroizing::new(bits_to_bytes(&bits));
    let mut pad = true;
    while data.len() < DATA_CODEWORDS {
        data.push(if pad { 0xec } else { 0x11 });
        pad = !pad;
    }

    let blocks = [
        Zeroizing::new(data[0..38].to_vec()),
        Zeroizing::new(data[38..76].to_vec()),
        Zeroizing::new(data[76..115].to_vec()),
        Zeroizing::new(data[115..154].to_vec()),
    ];
    let generator = rs_generator(ECC_PER_BLOCK);
    let ecc: Zeroizing<Vec<Zeroizing<Vec<u8>>>> = Zeroizing::new(
        blocks
            .iter()
            .map(|block| Zeroizing::new(rs_remainder(block, &generator)))
            .collect(),
    );
    let mut codewords = Zeroizing::new(Vec::with_capacity(242));
    for i in 0..39 {
        for block in &blocks {
            if i < block.len() {
                codewords.push(block[i]);
            }
        }
    }
    for i in 0..ECC_PER_BLOCK {
        for block in ecc.iter() {
            codewords.push(block[i]);
        }
    }

    let mut qr = Matrix::new();
    qr.draw_function_patterns();
    qr.place_data(&codewords);
    let mut best = None;
    let mut best_penalty = usize::MAX;
    for mask in 0..8 {
        let mut candidate = qr.clone();
        candidate.apply_mask(mask);
        candidate.draw_format(mask);
        let penalty = candidate.penalty();
        if penalty < best_penalty {
            best_penalty = penalty;
            best = Some(candidate);
        }
    }
    let modules = std::mem::take(&mut best?.modules)
        .into_iter()
        .map(u8::from)
        .collect();
    Some(Zeroizing::new(modules))
}

fn append_bits(out: &mut Vec<bool>, value: usize, count: usize) {
    for i in (0..count).rev() {
        out.push(((value >> i) & 1) != 0);
    }
}

fn bits_to_bytes(bits: &[bool]) -> Vec<u8> {
    bits.chunks(8)
        .map(|chunk| chunk.iter().fold(0, |v, b| (v << 1) | usize::from(*b)) as u8)
        .collect()
}

fn gf_mul(mut x: u8, mut y: u8) -> u8 {
    let mut z = 0;
    while y != 0 {
        if y & 1 != 0 {
            z ^= x;
        }
        y >>= 1;
        x = (x << 1) ^ (if x & 0x80 != 0 { 0x1d } else { 0 });
    }
    z
}

fn rs_generator(degree: usize) -> Vec<u8> {
    let mut result = vec![1];
    let mut root = 1;
    for _ in 0..degree {
        let mut next = vec![0; result.len() + 1];
        for (i, &coefficient) in result.iter().enumerate() {
            next[i] ^= coefficient;
            next[i + 1] ^= gf_mul(coefficient, root);
        }
        result = next;
        root = gf_mul(root, 2);
    }
    result
}

fn rs_remainder(data: &[u8], generator: &[u8]) -> Vec<u8> {
    let mut result = vec![0; generator.len() - 1];
    for &byte in data {
        let factor = byte ^ result.remove(0);
        result.push(0);
        for (slot, &coefficient) in result.iter_mut().zip(&generator[1..]) {
            *slot ^= gf_mul(coefficient, factor);
        }
    }
    result
}

#[derive(Clone)]
struct Matrix {
    modules: Vec<bool>,
    function: Vec<bool>,
    reserved: Vec<bool>,
}

impl Drop for Matrix {
    fn drop(&mut self) {
        self.modules.zeroize();
    }
}

impl Matrix {
    fn new() -> Self {
        Self {
            modules: vec![false; SIZE * SIZE],
            function: vec![false; SIZE * SIZE],
            reserved: vec![false; SIZE * SIZE],
        }
    }
    fn index(x: usize, y: usize) -> usize {
        y * SIZE + x
    }
    fn set(&mut self, x: usize, y: usize, dark: bool, function: bool) {
        let i = Self::index(x, y);
        self.modules[i] = dark;
        if function {
            self.function[i] = true;
        }
    }
    fn draw_finder(&mut self, cx: usize, cy: usize) {
        for dy in -4..=4 {
            for dx in -4..=4 {
                let x = cx as isize + dx;
                let y = cy as isize + dy;
                if x >= 0 && y >= 0 && x < SIZE as isize && y < SIZE as isize {
                    let dist = dx.abs().max(dy.abs());
                    self.set(x as usize, y as usize, dist != 2 && dist != 4, true);
                }
            }
        }
    }
    fn draw_function_patterns(&mut self) {
        self.draw_finder(3, 3);
        self.draw_finder(SIZE - 4, 3);
        self.draw_finder(3, SIZE - 4);
        for i in 8..(SIZE - 8) {
            self.set(i, 6, i % 2 == 0, true);
            self.set(6, i, i % 2 == 0, true);
        }
        for &cy in &[6usize, 24, 42] {
            for &cx in &[6usize, 24, 42] {
                let overlaps_finder = [(6, 6), (42, 6), (6, 42)].contains(&(cx, cy));
                if overlaps_finder {
                    continue;
                }
                for dy in -2..=2 {
                    for dx in -2..=2 {
                        self.set(
                            (cx as isize + dx) as usize,
                            (cy as isize + dy) as usize,
                            dx.abs().max(dy.abs()) != 1,
                            true,
                        );
                    }
                }
            }
        }
        self.draw_version();
        // Reserve format locations before placing payload bits.
        for y in [0usize, 1, 2, 3, 4, 5, 7, 8] {
            self.reserved[Self::index(8, y)] = true;
        }
        for x in [0usize, 1, 2, 3, 4, 5, 7, 8] {
            self.reserved[Self::index(x, 8)] = true;
        }
        for i in 0..8 {
            self.reserved[Self::index(SIZE - 1 - i, 8)] = true;
        }
        for i in 0..7 {
            self.reserved[Self::index(8, SIZE - 1 - i)] = true;
        }
        self.reserved[Self::index(8, SIZE - 8)] = true;
        self.set(8, SIZE - 8, true, true);
    }
    fn draw_version(&mut self) {
        let mut rem = VERSION;
        for _ in 0..12 {
            rem = (rem << 1) ^ ((rem >> 11) * 0x1f25);
        }
        let bits = (VERSION << 12) | rem;
        for i in 0..18 {
            let bit = ((bits >> i) & 1) != 0;
            let a = SIZE - 11 + i % 3;
            let b = i / 3;
            self.set(a, b, bit, true);
            self.set(b, a, bit, true);
        }
    }
    fn draw_format(&mut self, mask: usize) {
        let data = mask; // M has format-level bits 00.
        let mut rem = data;
        for _ in 0..10 {
            rem = (rem << 1) ^ ((rem >> 9) * 0x537);
        }
        let bits = ((data << 10) | rem) ^ 0x5412;
        for i in 0..=5 {
            self.modules[Self::index(8, i)] = ((bits >> i) & 1) != 0;
        }
        self.modules[Self::index(8, 7)] = ((bits >> 6) & 1) != 0;
        self.modules[Self::index(8, 8)] = ((bits >> 7) & 1) != 0;
        self.modules[Self::index(7, 8)] = ((bits >> 8) & 1) != 0;
        for i in 9..15 {
            self.modules[Self::index(14 - i, 8)] = ((bits >> i) & 1) != 0;
        }
        for i in 0..=7 {
            self.modules[Self::index(SIZE - 1 - i, 8)] = ((bits >> i) & 1) != 0;
        }
        for i in 8..15 {
            self.modules[Self::index(8, SIZE - 15 + i)] = ((bits >> i) & 1) != 0;
        }
        self.modules[Self::index(8, SIZE - 8)] = true;
    }
    fn place_data(&mut self, codewords: &[u8]) {
        let mut bit = 0;
        let mut right = SIZE - 1;
        while right >= 1 {
            if right == 6 {
                right -= 1;
            }
            for vert in 0..SIZE {
                let upward = ((right + 1) & 2) == 0;
                let y = if upward { SIZE - 1 - vert } else { vert };
                for offset in 0..2 {
                    let x = right - offset;
                    let i = Self::index(x, y);
                    if !self.function[i] && !self.reserved[i] && bit < codewords.len() * 8 {
                        self.modules[i] = ((codewords[bit / 8] >> (7 - bit % 8)) & 1) != 0;
                        bit += 1;
                    }
                }
            }
            if right < 2 {
                break;
            }
            right -= 2;
        }
    }
    fn apply_mask(&mut self, mask: usize) {
        for y in 0..SIZE {
            for x in 0..SIZE {
                let i = Self::index(x, y);
                if !self.function[i] && !self.reserved[i] {
                    let invert = match mask {
                        0 => (x + y) % 2 == 0,
                        1 => y % 2 == 0,
                        2 => x % 3 == 0,
                        3 => (x + y) % 3 == 0,
                        4 => (y / 2 + x / 3) % 2 == 0,
                        5 => (x * y) % 2 + (x * y) % 3 == 0,
                        6 => ((x * y) % 2 + (x * y) % 3) % 2 == 0,
                        _ => ((x + y) % 2 + (x * y) % 3) % 2 == 0,
                    };
                    self.modules[i] ^= invert;
                }
            }
        }
    }
    fn penalty(&self) -> usize {
        let mut score = 0;
        for axis in 0..2 {
            for line in 0..SIZE {
                let at = |i: usize| {
                    self.modules[if axis == 0 {
                        Self::index(i, line)
                    } else {
                        Self::index(line, i)
                    }]
                };
                let mut run = 1;
                let mut prev = at(0);
                for i in 1..SIZE {
                    let cur = at(i);
                    if cur == prev {
                        run += 1;
                        if run == 5 {
                            score += 3;
                        } else if run > 5 {
                            score += 1;
                        }
                    } else {
                        run = 1;
                        prev = cur;
                    }
                }
                for i in 0..SIZE.saturating_sub(10) {
                    let p = [
                        true, false, true, true, true, false, true, false, false, false, false,
                    ];
                    let q = [
                        false, false, false, false, true, false, true, true, true, false, true,
                    ];
                    if (0..11).all(|j| at(i + j) == p[j]) || (0..11).all(|j| at(i + j) == q[j]) {
                        score += 40;
                    }
                }
            }
        }
        for y in 0..SIZE - 1 {
            for x in 0..SIZE - 1 {
                let v = self.modules[Self::index(x, y)];
                if self.modules[Self::index(x + 1, y)] == v
                    && self.modules[Self::index(x, y + 1)] == v
                    && self.modules[Self::index(x + 1, y + 1)] == v
                {
                    score += 3;
                }
            }
        }
        let dark = self.modules.iter().filter(|&&v| v).count();
        score + (dark * 20).abs_diff(SIZE * SIZE * 10) / (SIZE * SIZE) * 10
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_eight_medium_profile_is_bounded_and_square() {
        let matrix = encode(&"x".repeat(MAX_BYTES)).unwrap();
        assert_eq!(matrix.len(), SIZE * SIZE);
        assert!(encode(&"x".repeat(MAX_BYTES + 1)).is_none());
        assert_eq!(
            encode("日本語 otpauth://totp/Sakura").unwrap().len(),
            SIZE * SIZE
        );
    }

    #[test]
    fn finder_alignment_timing_and_format_regions_are_reserved() {
        let mut matrix = Matrix::new();
        matrix.draw_function_patterns();
        assert!(matrix.modules[Matrix::index(0, 0)]);
        assert!(!matrix.modules[Matrix::index(1, 1)]);
        assert!(matrix.modules[Matrix::index(24, 24)]);
        assert!(matrix.function[Matrix::index(24, 24)]);
        assert!(matrix.function[Matrix::index(20, 6)]);
        assert!(matrix.reserved[Matrix::index(8, 0)]);
        assert!(matrix.reserved[Matrix::index(48, 8)]);
        assert!(matrix.reserved[Matrix::index(8, 48)]);
    }

    #[test]
    fn reed_solomon_generator_has_expected_degree_and_leading_term() {
        let generator = rs_generator(ECC_PER_BLOCK);
        assert_eq!(generator.len(), ECC_PER_BLOCK + 1);
        assert_eq!(generator[0], 1);
        assert_eq!(rs_remainder(&[0; 38], &generator).len(), ECC_PER_BLOCK);
    }
}
