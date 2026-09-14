use super::super::{
    Surface, BASE_DICTIONARY_EDGES_PER_READING, MAX_DICTIONARY_SURFACES_PER_READING,
};

/// The per-reading-length cap on dictionary edges: the historical baseline
/// rows first, then one edge per distinct surface up to
/// [`MAX_DICTIONARY_SURFACES_PER_READING`].
///
/// The seen-surface set is converter-owned scratch, not a stack local. It used
/// to be an inline `[u32; MAX_DICTIONARY_SURFACES_PER_READING]`, which was 64
/// bytes while that bound was 12 and became 1,040 bytes when #94/#95 tied it
/// to `MAX_CONVERSION_CANDIDATES`. A `Copy` struct that large is materialised
/// once per `new` and once per move in an unoptimised build, and `build_lattice`
/// constructed one for every reading start. The reservation this spends is the
/// conversion worker's stack, which `worker_locals_fit_the_reserved_stack` in
/// `sakura-engine` bounds and which multiplies by `MAX_INSTANCES` threads.
///
/// Both constraints have to hold at once: keep this struct a few words wide so
/// the lattice frame stays small, and keep the capacity in the converter's
/// process-lifetime arena so the conversion path itself never allocates —
/// `conversion_into_reused_candidate_buffers_allocates_nothing` in
/// `sakura-engine` fails the moment a reading start reserves a fresh one.
#[derive(Debug)]
pub(in crate::conversion) struct DictionaryEdgeBudget {
    baseline_edges: usize,
    surfaces: Vec<u32>,
}

impl DictionaryEdgeBudget {
    /// Called once per converter slot, off the conversion path.
    pub(in crate::conversion) fn new() -> Self {
        Self {
            baseline_edges: 0,
            surfaces: Vec::with_capacity(MAX_DICTIONARY_SURFACES_PER_READING),
        }
    }

    pub(in crate::conversion) fn reset(&mut self) {
        self.baseline_edges = 0;
        // `clear` keeps the capacity reserved in `new`, and `admit` only ever
        // pushes below `MAX_DICTIONARY_SURFACES_PER_READING`, so no reading
        // start can force a reallocation.
        self.surfaces.clear();
    }

    pub(in crate::conversion) fn admit(&mut self, surface_id: u32) -> bool {
        let known_surface = self.surfaces.contains(&surface_id);
        if self.baseline_edges < BASE_DICTIONARY_EDGES_PER_READING {
            self.baseline_edges += 1;
            if !known_surface && self.surfaces.len() < MAX_DICTIONARY_SURFACES_PER_READING {
                self.surfaces.push(surface_id);
            }
            return true;
        }
        if known_surface || self.surfaces.len() >= MAX_DICTIONARY_SURFACES_PER_READING {
            return false;
        }
        self.surfaces.push(surface_id);
        true
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::conversion) struct Node {
    pub(in crate::conversion) start: usize,
    pub(in crate::conversion) end: usize,
    pub(in crate::conversion) left_id: u16,
    pub(in crate::conversion) right_id: u16,
    pub(in crate::conversion) local_cost: i64,
    pub(in crate::conversion) best_cost: i64,
    pub(in crate::conversion) best_previous: usize,
    pub(in crate::conversion) suffix_cost: i64,
    pub(in crate::conversion) next_from_start: usize,
    pub(in crate::conversion) next_at_end: usize,
    pub(in crate::conversion) surface: Surface,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::conversion) struct NodeSpec {
    pub(in crate::conversion) start: usize,
    pub(in crate::conversion) end: usize,
    pub(in crate::conversion) left_id: u16,
    pub(in crate::conversion) right_id: u16,
    pub(in crate::conversion) local_cost: i64,
    pub(in crate::conversion) surface: Surface,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(in crate::conversion) enum CharClass {
    Hiragana,
    Katakana,
    AsciiDigit,
    AsciiLetter,
    Other,
}

pub(in crate::conversion) fn char_class(character: char) -> CharClass {
    match character {
        '\u{3040}'..='\u{309f}' | 'ー' => CharClass::Hiragana,
        '\u{30a0}'..='\u{30ff}' | '\u{31f0}'..='\u{31ff}' => CharClass::Katakana,
        '0'..='9' => CharClass::AsciiDigit,
        'A'..='Z' | 'a'..='z' => CharClass::AsciiLetter,
        _ => CharClass::Other,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::conversion) struct CharRun {
    pub(in crate::conversion) end: usize,
    pub(in crate::conversion) chars: usize,
}

pub(in crate::conversion) fn char_run(reading: &str, start: usize) -> CharRun {
    let mut characters = reading[start..].char_indices();
    let Some((_, first)) = characters.next() else {
        return CharRun {
            end: start,
            chars: 0,
        };
    };
    let class = char_class(first);
    let mut end = start + first.len_utf8();
    let mut count = 1usize;
    for (relative, character) in characters {
        if char_class(character) != class {
            break;
        }
        end = start + relative + character.len_utf8();
        count += 1;
    }
    CharRun { end, chars: count }
}
