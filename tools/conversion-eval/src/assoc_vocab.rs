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

use std::collections::{BTreeMap, HashMap};

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
        let mut nodes = vec![Node::default()];
        for entry in &mut entries {
            let homophones = &surfaces_of_reading[&entry.reading];
            entry.is_target = homophones.len() >= 2 && has_kanji_or_katakana(&entry.surface);
            entry.is_clue = has_kanji_or_katakana(&entry.surface);
            by_surface
                .entry(entry.surface.clone())
                .or_default()
                .push(entry.index);
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
            root: 0,
            nodes,
        })
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
/// `counts[(target, clue, distance)]` is one bucket; `distance` is the char
/// gap between the two tokens' nearest boundaries (before: clue_end ->
/// target_start, after: target_end -> clue_start), capped at 127 so every
/// window W <= 128 can be emitted from the same counts.
#[derive(Debug, Default)]
#[allow(dead_code)] // read by assoc-table-build only
pub struct Counts {
    /// (target entry, clue entry, distance) -> occurrences.
    pub pairs: BTreeMap<(u32, u32, u8), u64>,
    /// Articles processed, for the report.
    pub articles: u64,
    /// Paragraphs processed, for the report.
    pub paragraphs: u64,
}

/// Distance cap: counts store min(gap, DISTANCE_CAP); a window `W` includes
/// every pair with gap < W, so stored counts support W <= DISTANCE_CAP + 1.
#[allow(dead_code)] // read by assoc-table-build only
pub const DISTANCE_CAP: u8 = 127;

/// Counts `text` (paragraphs separated by newlines, articles by form feeds)
/// into `counts` under the Issue's rules.
#[allow(dead_code)] // read by assoc-table-build only
pub fn count_text(vocab: &Vocab, counts: &mut Counts, text: &str) {
    for article in text.split('\u{0c}') {
        counts.articles += 1;
        for paragraph in article.lines() {
            if paragraph.is_empty() {
                continue;
            }
            counts.paragraphs += 1;
            count_paragraph(vocab, counts, paragraph);
        }
    }
}

/// One paragraph: tokenize, then for every target token look `DISTANCE_CAP`
/// characters each way and count the clue words found there.
fn count_paragraph(vocab: &Vocab, counts: &mut Counts, paragraph: &str) {
    let tokens = vocab.segment(paragraph);
    for (position, token) in tokens.iter().enumerate() {
        let Some(target) = token.entry else { continue };
        if !vocab.entries[target as usize].is_target {
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
            let Some(clue) = other.entry else { continue };
            if !vocab.entries[clue as usize].is_clue {
                continue;
            }
            if !counts_as_clue(vocab, &tokens, other_at) {
                continue;
            }
            let gap = if other.end <= token.start {
                token.start - other.end
            } else {
                other.start - token.end
            } as u8;
            *counts.pairs.entry((target, clue, gap)).or_insert(0) += 1;
        }
    }
}

/// A single-kanji clue counts only when its neighbours are not kanji (#474
/// counting rule). Longer clue words always count.
#[allow(dead_code)] // read by assoc-table-build only
fn counts_as_clue(vocab: &Vocab, tokens: &[Token], at: usize) -> bool {
    let token = tokens[at];
    let Some(entry) = token.entry else {
        return false;
    };
    let surface = &vocab.entries[entry as usize].surface;
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
