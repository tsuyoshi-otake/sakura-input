//! Shared vocabulary machinery for the #474 association prototype.
//!
//! `assoc-table-build` counts co-occurrences in public Wikipedia text and
//! `assoc-eval` collects clue words out of a committed-context tail; both
//! must segment text and classify words under exactly the same rules, so the
//! code lives once here. Everything keys on the shipped dictionary's own
//! entries: an entry index is the word identity, so counts and the table
//! stay stable across runs against the same `system.dic`.
//!
//! The segmentation rule is the Issue's, not a morphological analyzer: the
//! longest dictionary surface wins at each position. A surface qualifies as
//! a *target word* when its reading has at least one other surface and it
//! contains kanji or katakana; as a *clue word* when it contains kanji or
//! katakana (a single kanji only counts when its neighbours are not kanji,
//! per the Issue's counting rule, enforced at use sites where neighbours
//! are known).

use std::collections::HashMap;

use sakura_core::dictionary::{Dictionary, EntryFlags};

/// One dictionary entry the prototype can see.
#[derive(Debug, Clone)]
pub struct VocabEntry {
    /// Index into the dictionary's own entry table — stable per dictionary.
    pub index: u32,
    /// Surface text.
    pub surface: String,
    /// Reading text.
    pub reading: String,
    /// word_cost from the dictionary entry.
    #[allow(dead_code)] // read by assoc-eval, not assoc-table-build
    pub word_cost: i32,
    /// Conneciton ids for price reconstruction.
    pub left_id: u16,
    /// Connection ids for price reconstruction.
    pub right_id: u16,
    /// Entry flags (IT etc.) — kept for provenance-sensitive consumers.
    #[allow(dead_code)] // read by assoc-eval, not assoc-table-build
    pub flags: EntryFlags,
    /// This surface shares its reading with at least one other surface and
    /// contains kanji or katakana.
    pub is_target: bool,
    /// Contains kanji or katakana (a 1-char kanji is filtered by context at
    /// the call site).
    pub is_clue: bool,
}

/// The vocabulary a run works over.
pub struct Vocab {
    /// Entries in dictionary order.
    pub entries: Vec<VocabEntry>,
    /// Surface text -> entry indices that carry it.
    #[allow(dead_code)] // read by assoc-eval, not assoc-table-build
    by_surface: HashMap<String, Vec<u32>>,
    /// Entry -> the canonical target for counting: the cheapest entry
    /// sharing its (reading, surface), or u32::MAX when the word is not a
    /// counted target under the competition rules.
    target_of: Vec<u32>,
    /// Entry -> the canonical clue for counting: the cheapest entry
    /// sharing its surface, or u32::MAX when the surface is not a clue.
    clue_of: Vec<u32>,
    /// Longest-match trie over every dictionary surface.
    root: usize,
    nodes: Vec<Node>,
}

#[derive(Default)]
struct Node {
    children: HashMap<char, usize>,
    /// Dictionary entry index of the surface ending here (longest unique
    /// surface per node; surfaces shared by several entries keep the first).
    entry: Option<u32>,
}

/// Price band for real competition: an association bonus is capped at 3000
/// in the grid, so a word farther than this from its reading's cheapest can
/// never be swapped to the top by the measured rules.
pub const TARGET_BAND: i32 = 3000;

/// How many distinct clue surfaces are counted — the cheapest surfaces as a
/// frequency proxy (dictionary cost). Chosen inside the owner's "数万語".
pub const CLUE_CAP: usize = 60_000;

fn has_kanji_or_katakana(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            c,
            '\u{3400}'..='\u{4DBF}'
                | '\u{4E00}'..='\u{9FFF}'
                | '\u{F900}'..='\u{FAFF}'
                | '々'
                | '\u{30A0}'..='\u{30FF}'
                | '\u{31F0}'..='\u{31FF}'
        )
    })
}

impl Vocab {
    /// Loads every dictionary entry, flags target and clue words, and builds
    /// the longest-match surface trie.
    pub fn load(dictionary: &Dictionary<'_>) -> Result<Self, String> {
        let mut entries = Vec::new();
        let mut surfaces_of_reading: HashMap<String, Vec<u32>> = HashMap::new();
        dictionary
            .visit_entries(|reading, entry| {
                let mut surface = String::new();
                dictionary
                    .write_surface(entry, &mut surface)
                    .expect("surface write into String cannot overflow");
                let index = entries.len() as u32;
                surfaces_of_reading
                    .entry(reading.to_owned())
                    .or_default()
                    .push(index);
                entries.push(VocabEntry {
                    index,
                    surface,
                    reading: reading.to_owned(),
                    word_cost: entry.word_cost,
                    left_id: entry.left_id,
                    right_id: entry.right_id,
                    flags: entry.flags,
                    is_target: false,
                    is_clue: false,
                });
                true
            })
            .map_err(|error| format!("dictionary entry walk failed: {error:?}"))?;

        let mut by_surface: HashMap<String, Vec<u32>> = HashMap::new();
        for entry in &mut entries {
            by_surface
                .entry(entry.surface.clone())
                .or_default()
                .push(entry.index);
        }

        // Target rule (memory-bounded): within one reading, keep only the
        // entries that genuinely compete — surface clusters whose cheapest
        // entry costs at most TARGET_BAND more than the reading's cheapest.
        // A reading keeps targets only when at least two surfaces survive.
        let mut kept_targets = 0usize;
        let mut mark_targets: Vec<u32> = Vec::new();
        let mut target_of = vec![u32::MAX; entries.len()];
        for homophones in surfaces_of_reading.values() {
            let group_min = homophones
                .iter()
                .map(|&index| entries[index as usize].word_cost)
                .min()
                .unwrap_or(i32::MAX);
            // Canonical entry per (reading, surface): the cheapest member.
            let mut canonical: HashMap<&str, u32> = HashMap::new();
            for &index in homophones {
                let entry = &entries[index as usize];
                if !has_kanji_or_katakana(&entry.surface) {
                    continue;
                }
                canonical
                    .entry(entry.surface.as_str())
                    .and_modify(|best| {
                        if entry.word_cost < entries[*best as usize].word_cost {
                            *best = index;
                        }
                    })
                    .or_insert(index);
            }
            let kept: Vec<u32> = canonical
                .values()
                .copied()
                .filter(|&index| entries[index as usize].word_cost <= group_min + TARGET_BAND)
                .collect();
            if kept.len() < 2 {
                continue;
            }
            for &index in homophones {
                let entry = &entries[index as usize];
                if let Some(&best) = canonical.get(entry.surface.as_str()) {
                    if kept.contains(&best) {
                        target_of[index as usize] = best;
                        mark_targets.push(best);
                    }
                }
            }
            kept_targets += kept.len();
        }
        for best in mark_targets {
            entries[best as usize].is_target = true;
        }

        // Clue rule (memory-bounded): the CLUE_CAP cheapest distinct
        // kanji/katakana surfaces; one canonical entry per surface.
        let mut clue_surfaces: Vec<(i32, &str, u32)> = Vec::new();
        for indices in by_surface.values() {
            let canonical = *indices
                .iter()
                .min_by_key(|&&index| (entries[index as usize].word_cost, index))
                .expect("surface cluster is never empty");
            let entry = &entries[canonical as usize];
            if has_kanji_or_katakana(&entry.surface) {
                clue_surfaces.push((entry.word_cost, entry.surface.as_str(), canonical));
            }
        }
        clue_surfaces.sort();
        let kept_canonicals: Vec<u32> = clue_surfaces
            .iter()
            .take(CLUE_CAP)
            .map(|&(_, _, canonical)| canonical)
            .collect();
        drop(clue_surfaces);
        let mut clue_of = vec![u32::MAX; entries.len()];
        let mut kept_clues: std::collections::HashSet<u32> = std::collections::HashSet::new();
        for canonical in kept_canonicals {
            kept_clues.insert(canonical);
            entries[canonical as usize].is_clue = true;
        }
        for indices in by_surface.values() {
            let canonical = *indices
                .iter()
                .min_by_key(|&&index| (entries[index as usize].word_cost, index))
                .expect("surface cluster is never empty");
            if kept_clues.contains(&canonical) {
                for &index in indices {
                    clue_of[index as usize] = canonical;
                }
            }
        }
        eprintln!(
            "vocab kept: {kept_targets} targets, {} clue surfaces",
            kept_clues.len()
        );

        let mut nodes = vec![Node::default()];
        for entry in &mut entries {
            let mut node = 0usize;
            for ch in entry.surface.chars() {
                node = match nodes[node].children.get(&ch) {
                    Some(&next) => next,
                    None => {
                        nodes.push(Node::default());
                        let next = nodes.len() - 1;
                        nodes[node].children.insert(ch, next);
                        next
                    }
                };
            }
            if nodes[node].entry.is_none() {
                nodes[node].entry = Some(entry.index);
            }
        }
        Ok(Self {
            entries,
            by_surface,
            target_of,
            clue_of,
            root: 0,
            nodes,
        })
    }

    /// The canonical clue entry behind a trie-matched entry, if the surface
    /// is a counted clue.
    #[allow(dead_code)] // read by assoc-eval, not assoc-table-build
    pub fn clue_entry(&self, entry: u32) -> Option<u32> {
        match self.clue_of[entry as usize] {
            u32::MAX => None,
            canonical => Some(canonical),
        }
    }

    /// Entry indices that carry `surface` (several entries can share one).
    #[allow(dead_code)] // read by assoc-eval, not assoc-table-build
    pub fn lookup(&self, surface: &str) -> &[u32] {
        self.by_surface
            .get(surface)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The dictionary entry whose (reading, surface, left/right id) matches —
    /// the single segment of a one-edge system candidate resolves to exactly
    /// one entry, which is what `assoc-eval` needs to recompute its price.
    #[allow(dead_code)] // read by assoc-eval, not assoc-table-build
    pub fn entry_for(
        &self,
        reading: &str,
        surface: &str,
        left_id: u16,
        right_id: u16,
    ) -> Option<&VocabEntry> {
        self.by_surface.get(surface)?.iter().find_map(|&index| {
            let entry = &self.entries[index as usize];
            (entry.reading == reading && entry.left_id == left_id && entry.right_id == right_id)
                .then_some(entry)
        })
    }

    /// Longest-match segmentation of `text`. Each token reports its char
    /// span, the entry index behind the matched surface, and its flags.
    /// Unmatched spans become `None`-entry tokens so clue windows measured
    /// in characters stay truthful.
    pub fn segment(&self, text: &str) -> Vec<Token> {
        let mut tokens = Vec::new();
        let mut char_at = 0usize;
        for piece in text.split_inclusive('\n') {
            let _ = piece;
        }
        let chars: Vec<(usize, char)> = text.char_indices().collect();
        let total = chars.len();
        while char_at < total {
            // Longest surface match starting at char_at.
            let mut node = self.root;
            let mut best: Option<(usize, u32)> = None;
            let mut scan = char_at;
            while scan < total {
                let ch = chars[scan].1;
                match self.nodes[node].children.get(&ch) {
                    Some(&next) => {
                        node = next;
                        if let Some(entry) = self.nodes[node].entry {
                            best = Some((scan + 1, entry));
                        }
                        scan += 1;
                    }
                    None => break,
                }
            }
            match best {
                Some((end, entry)) => {
                    tokens.push(Token {
                        start: char_at,
                        end,
                        entry: Some(entry),
                    });
                    char_at = end;
                }
                None => {
                    tokens.push(Token {
                        start: char_at,
                        end: char_at + 1,
                        entry: None,
                    });
                    char_at += 1;
                }
            }
        }
        tokens
    }
}

/// One segmented token of input text.
#[derive(Debug, Clone, Copy)]
pub struct Token {
    /// Char offset where the token starts.
    pub start: usize,
    /// Char offset where it ends.
    pub end: usize,
    /// The dictionary entry whose surface matched, if any.
    pub entry: Option<u32>,
}

/// The co-occurrence counters shared by the build and its checkpoint file.
/// Packs (target canonical entry, clue canonical entry, distance bucket)
/// into one u64 key: ids stay below 2^22, the bucket below 4.
#[allow(dead_code)] // read by assoc-table-build only
pub fn pack_key(target: u32, clue: u32, bucket: u8) -> u64 {
    ((target as u64) << 24) | ((clue as u64) << 2) | u64::from(bucket)
}

/// Inverse of `pack_key`.
#[allow(dead_code)] // read by assoc-table-build only
pub fn unpack_key(key: u64) -> (u32, u32, u8) {
    (
        (key >> 24) as u32,
        ((key >> 2) & 0x3f_ffff) as u32,
        (key & 3) as u8,
    )
}

/// Small deterministic multiply-xor hasher for the packed u64 keys — the
/// map sees tens of millions of inserts, and a local measurement tool has
/// no adversarial-input concern.
#[derive(Default)]
#[allow(dead_code)] // read by assoc-table-build only
pub struct PairHasher(u64);

impl std::hash::Hasher for PairHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn write_u64(&mut self, value: u64) {
        self.0 = (self.0 ^ value)
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            .rotate_left(23);
    }
}

/// `counts[(target, clue, distance_bucket)]` is one bucket; the gap between
/// the two tokens' nearest boundaries is folded into the three tiers the
/// grid needs (0: gap < 30, 1: gap < 60, 2: gap < 120), so one counts file
/// serves windows 30, 60 and 120 while keeping the map small enough to hold.
#[derive(Debug, Default)]
#[allow(dead_code)] // read by assoc-table-build only
pub struct Counts {
    /// pack_key(target, clue, bucket) -> occurrences; HashMap keeps one
    /// entry per key without per-node tree overhead.
    pub pairs: HashMap<u64, u64, std::hash::BuildHasherDefault<PairHasher>>,
    /// Articles processed, for the report.
    pub articles: u64,
    /// Paragraphs processed, for the report.
    pub paragraphs: u64,
}

/// Distance cap: counting looks DISTANCE_CAP+1 characters each way.
#[allow(dead_code)] // read by assoc-table-build only
pub const DISTANCE_CAP: u8 = 127;

/// Distance tier for a gap: 0 = under 30, 1 = under 60, 2 = under 120;
/// gaps of 120+ return None (outside the largest grid window). A window W
/// keeps every bucket fully below W: 30 -> {0}, 60 -> {0,1}, 120 -> {0,1,2}.
#[allow(dead_code)] // read by assoc-table-build only
pub fn distance_bucket(gap: usize) -> Option<u8> {
    if gap < 30 {
        Some(0)
    } else if gap < 60 {
        Some(1)
    } else if gap < 120 {
        Some(2)
    } else {
        None
    }
}

/// One paragraph: tokenize, then for every target token look `DISTANCE_CAP`
/// characters each way and count the clue words found there.
#[allow(dead_code)] // read by assoc-table-build only
pub fn count_paragraph(vocab: &Vocab, counts: &mut Counts, paragraph: &str) {
    let tokens = vocab.segment(paragraph);
    for (position, token) in tokens.iter().enumerate() {
        let Some(entry) = token.entry else { continue };
        let target = vocab.target_of[entry as usize];
        if target == u32::MAX {
            continue;
        }
        let lo = token.start.saturating_sub(usize::from(DISTANCE_CAP) + 1);
        let hi = token.end + usize::from(DISTANCE_CAP) + 1;
        for (other_at, other) in tokens.iter().enumerate() {
            if other_at == position || other.start > hi || other.end <= lo {
                continue;
            }
            if other.start >= token.start && other.end <= token.end {
                continue;
            }
            let Some(other_entry) = other.entry else {
                continue;
            };
            let clue = vocab.clue_of[other_entry as usize];
            if clue == u32::MAX {
                continue;
            }
            if !counts_as_clue(vocab, &tokens, other_at, clue) {
                continue;
            }
            let gap = if other.end <= token.start {
                token.start - other.end
            } else {
                other.start - token.end
            };
            let Some(bucket) = distance_bucket(gap) else {
                continue;
            };
            *counts
                .pairs
                .entry(pack_key(target, clue, bucket))
                .or_insert(0) += 1;
        }
    }
}

/// A single-kanji clue counts only when its neighbours are not kanji (#474
/// counting rule). `canonical` is the clue's canonical surface entry; longer
/// clue words always count.
#[allow(dead_code)] // read by assoc-table-build only
fn counts_as_clue(vocab: &Vocab, tokens: &[Token], at: usize, canonical: u32) -> bool {
    let token = tokens[at];
    let surface = &vocab.entries[canonical as usize].surface;
    if surface.chars().count() != 1 || !is_kanji(surface.chars().next().unwrap_or('\0')) {
        return true;
    }
    let left_is_kanji =
        at > 0 && tokens[at - 1].end == token.start && token_is_kanji(vocab, tokens[at - 1]);
    let right_is_kanji = at + 1 < tokens.len()
        && tokens[at + 1].start == token.end
        && token_is_kanji(vocab, tokens[at + 1]);
    !(left_is_kanji || right_is_kanji)
}

#[allow(dead_code)] // read by assoc-table-build only
fn token_is_kanji(vocab: &Vocab, token: Token) -> bool {
    token
        .entry
        .map(|index| vocab.entries[index as usize].surface.chars().all(is_kanji))
        .unwrap_or(false)
}

#[allow(dead_code)] // read by assoc-table-build only
fn is_kanji(c: char) -> bool {
    matches!(
        c,
        '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' | '々'
    )
}
