//! Which word owns a glossary reading in the pre-overlay dictionary.
//!
//! The glossary may take rank two, never rank one, from such a word (Issue
//! #282). Ownership is decided here, against the upstream edges alone, so the
//! glossary pricing in the parent module needs to know only the owner's
//! surface and the lattice total it ranks first with. The glossary's own
//! edges take part only as pieces of a longer reading's path.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use crate::{ConnectionMatrix, SourceEntry};
use sakura_core::conversion::ConversionOptions;
use sakura_core::dictionary::EntryFlags;

/// Mozc's beginning- and end-of-sentence connection class.
const SENTENCE_BOUNDARY_CLASS: u16 = 0;

/// The word a context-free conversion of a reading ranks first before the
/// glossary overlay exists, and the lattice total it ranks first with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Owner {
    pub(super) surface: String,
    pub(super) total: i64,
}

/// Upstream edges on every glossary reading and on every substring of one,
/// collected from the streamed pre-overlay dictionary (the trimmed Mozc
/// lexicon and its generated inflections), and the glossary's own edges.
#[derive(Debug, Default)]
pub(super) struct ReadingOwners {
    pieces: BTreeSet<String>,
    edges: BTreeMap<String, Vec<LatticeEdge>>,
    overlay: BTreeMap<String, Vec<LatticeEdge>>,
}

#[derive(Debug, Clone)]
struct LatticeEdge {
    surface: String,
    left_id: u16,
    right_id: u16,
    word_cost: i32,
    it: bool,
    non_initial: bool,
}

impl LatticeEdge {
    fn word_total(&self, options: &ConversionOptions) -> i64 {
        let it_reduction = if self.it {
            options.it_boost(self.word_cost)
        } else {
            0
        };
        i64::from(self.word_cost).saturating_sub(it_reduction)
    }
}

impl ReadingOwners {
    /// Prepares to collect the edges that can form a path over `readings`.
    pub(super) fn new<'a>(readings: impl IntoIterator<Item = &'a str>) -> Self {
        let mut pieces = BTreeSet::new();
        for reading in readings {
            let starts: Vec<usize> = reading.char_indices().map(|(index, _)| index).collect();
            for (offset, &start) in starts.iter().enumerate() {
                for &end in starts[offset + 1..].iter().chain([&reading.len()]) {
                    pieces.insert(reading[start..end].to_owned());
                }
            }
        }
        Self {
            pieces,
            edges: BTreeMap::new(),
            overlay: BTreeMap::new(),
        }
    }

    /// Retains the upstream entries whose reading is a glossary reading or a
    /// substring of one. Spelling corrections are not conversions of what
    /// was typed, so they never own a reading and are not retained.
    pub(super) fn observe(&mut self, entries: &[SourceEntry]) {
        for entry in entries {
            if entry.flags.contains(EntryFlags::SPELLING_CORRECTION)
                || !self.pieces.contains(entry.reading.as_str())
            {
                continue;
            }
            self.edges
                .entry(entry.reading.clone())
                .or_default()
                .push(LatticeEdge {
                    surface: entry.surface.clone(),
                    left_id: entry.left_id,
                    right_id: entry.right_id,
                    word_cost: entry.word_cost,
                    it: entry.flags.contains(EntryFlags::IT),
                    non_initial: entry.flags.contains(EntryFlags::NON_INITIAL),
                });
        }
    }

    /// Retains one priced glossary edge. Every glossary reading is a piece
    /// of itself, so all of them are kept; like every glossary edge it is
    /// tagged `IT` and may start a conversion.
    pub(super) fn observe_overlay(
        &mut self,
        reading: &str,
        surface: &str,
        left_id: u16,
        right_id: u16,
        word_cost: i32,
    ) {
        self.overlay
            .entry(reading.to_owned())
            .or_default()
            .push(LatticeEdge {
                surface: surface.to_owned(),
                left_id,
                right_id,
                word_cost,
                it: true,
                non_initial: false,
            });
    }

    /// The owner of `reading`, if it has one. The candidate is the cheapest
    /// upstream edge that spells the whole reading and may start a
    /// conversion; it owns the reading only when it is an owning word (see
    /// [`owns_reading`]) and no cheaper lattice path over the upstream edges
    /// spells something else. A path that splits the reading into fragments
    /// (`た今` for `たいま`) is what the user would see first once the glossary
    /// stepped back, so such a reading has no owner to step back for. That
    /// path may use glossary edges on a shorter part of the reading
    /// (`か`+`ラム` for `からむ`), priced before their own reading yields:
    /// a yield only raises a cost, so this never invents an owner. A
    /// cheaper split with the owner's own spelling (`図り`+`やすい`) still
    /// shows the owner first, and its total is the one returned.
    pub(super) fn owner(
        &self,
        reading: &str,
        connection: &ConnectionMatrix,
        pos_features: &[String],
        options: &ConversionOptions,
    ) -> Option<Owner> {
        let (leader_total, leader) = self
            .edges
            .get(reading)?
            .iter()
            .filter(|edge| !edge.non_initial)
            .map(|edge| {
                let total = standalone_total(
                    connection,
                    edge.left_id,
                    edge.right_id,
                    edge.word_total(options),
                );
                (total, edge)
            })
            .min_by(|(left, a), (right, b)| {
                left.cmp(right).then_with(|| a.surface.cmp(&b.surface))
            })?;
        if !owns_reading(reading, leader, pos_features) {
            return None;
        }
        match self.cheapest_path(reading, connection, options) {
            Some((total, surface)) if total < leader_total => {
                (surface == leader.surface).then_some(Owner { surface, total })
            }
            _ => Some(Owner {
                surface: leader.surface.clone(),
                total: leader_total,
            }),
        }
    }

    /// The cheapest lattice path over the retained edges that spells
    /// `reading`, with its total and surface; glossary edges spelling the
    /// whole reading are the ones being ranked and take no part. Ties keep
    /// the path found first, which is deterministic because the edges are
    /// visited in reading and observation order.
    fn cheapest_path(
        &self,
        reading: &str,
        connection: &ConnectionMatrix,
        options: &ConversionOptions,
    ) -> Option<(i64, String)> {
        /// The boundary and right class a path step continues from.
        type From = Option<(usize, u16)>;
        struct Step<'a> {
            total: i64,
            from: From,
            edge: &'a LatticeEdge,
        }
        let mut boundaries: Vec<usize> = reading.char_indices().map(|(index, _)| index).collect();
        boundaries.push(reading.len());
        // `best[position][right_id]` is the cheapest path ending at that
        // boundary with that right class.
        let mut best: Vec<BTreeMap<u16, Step<'_>>> =
            boundaries.iter().map(|_| BTreeMap::new()).collect();
        for start in 0..boundaries.len() - 1 {
            let arrivals: Vec<(From, u16, i64)> = if start == 0 {
                vec![(None, SENTENCE_BOUNDARY_CLASS, 0)]
            } else {
                best[start]
                    .iter()
                    .map(|(&right_id, step)| (Some((start, right_id)), right_id, step.total))
                    .collect()
            };
            if arrivals.is_empty() {
                continue;
            }
            for end in start + 1..boundaries.len() {
                let piece = &reading[boundaries[start]..boundaries[end]];
                let whole = start == 0 && end == boundaries.len() - 1;
                let upstream = self.edges.get(piece).into_iter().flatten();
                let overlay = self
                    .overlay
                    .get(piece)
                    .filter(|_| !whole)
                    .into_iter()
                    .flatten();
                for edge in upstream
                    .chain(overlay)
                    .filter(|edge| start != 0 || !edge.non_initial)
                {
                    for &(from, right_id, total) in &arrivals {
                        let total = total
                            .saturating_add(link(connection, right_id, edge.left_id))
                            .saturating_add(edge.word_total(options));
                        match best[end].entry(edge.right_id) {
                            Entry::Occupied(mut held) => {
                                if total < held.get().total {
                                    held.insert(Step { total, from, edge });
                                }
                            }
                            Entry::Vacant(empty) => {
                                empty.insert(Step { total, from, edge });
                            }
                        }
                    }
                }
            }
        }
        let last = boundaries.len() - 1;
        let (&right_id, step) = best[last].iter().min_by_key(|(&right_id, step)| {
            step.total
                .saturating_add(link(connection, right_id, SENTENCE_BOUNDARY_CLASS))
        })?;
        let total = step
            .total
            .saturating_add(link(connection, right_id, SENTENCE_BOUNDARY_CLASS));
        let mut pieces = vec![step.edge.surface.as_str()];
        let mut from = step.from;
        while let Some((position, right_id)) = from {
            let step = &best[position][&right_id];
            pieces.push(step.edge.surface.as_str());
            from = step.from;
        }
        pieces.reverse();
        Some((total, pieces.concat()))
    }
}

/// Lattice total of one edge converted alone: sentence-start connection,
/// word cost after any IT reduction, sentence-end connection.
pub(super) fn standalone_total(
    connection: &ConnectionMatrix,
    left_id: u16,
    right_id: u16,
    word_total: i64,
) -> i64 {
    link(connection, SENTENCE_BOUNDARY_CLASS, left_id)
        .saturating_add(word_total)
        .saturating_add(link(connection, right_id, SENTENCE_BOUNDARY_CLASS))
}

/// An unknown class pair costs `u16::MAX`, as it does at runtime.
fn link(connection: &ConnectionMatrix, right_id: u16, left_id: u16) -> i64 {
    i64::from(connection.cost(right_id, left_id).unwrap_or(u16::MAX))
}

/// Whether a pre-overlay leader is a word that owns its reading rather than
/// a fragment that leads it only because the reading is short. The leader's
/// left class names its first morpheme, so a particle- or auxiliary-initial
/// compound (`でグレード`) fails with the particle. Suffixes, prefixes,
/// dependent (`非自立`) forms, interjections, fillers and symbols do not own a
/// reading either. A verb or adjective owns it only in a form that can stand
/// alone, the dictionary or continuative form (`塗る`, `関し`), not one that
/// needs a following word (`転ん`, the `悪` of `悪がる`). A proper noun shares
/// the reading by coincidence of name (`双子座` for `じぇみに`), not as the
/// vocabulary this rule protects. A noun's hiragana echo (`ぷる`) is a
/// spelling fallback, not the noun; adverbs, verbs and adjectives are
/// ordinarily written in kana, so their echo (`ようやく`) does own the reading.
fn owns_reading(reading: &str, leader: &LatticeEdge, pos_features: &[String]) -> bool {
    let Some(feature) = pos_features.get(usize::from(leader.left_id)) else {
        return false;
    };
    let fields: Vec<&str> = feature.split(',').collect();
    let field = |index: usize| fields.get(index).copied().unwrap_or_default();
    let (head, detail, form) = (field(0), field(1), field(5));
    matches!(
        head,
        "名詞" | "動詞" | "形容詞" | "副詞" | "連体詞" | "接続詞"
    ) && !matches!(detail, "接尾" | "非自立" | "固有名詞")
        && (!matches!(head, "動詞" | "形容詞") || form == "連用形" || form.contains("基本形"))
        && !(head == "名詞" && leader.surface == reading)
}
