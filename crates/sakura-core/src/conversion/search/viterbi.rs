use core::cmp::Ordering;

use sakura_values::MAX_SEGMENTS;

use crate::dictionary::Dictionary;
use crate::user_dictionary::UserDictionary;

use super::super::{
    connection_cost, has_same_surface, make_candidate, CandidateMaterialization, ConversionError,
    ConversionSearchTerminal, Converter, LeftContextId, RightContextId, Surface, NONE, NONE_STATE,
};
use super::Node;

#[derive(Debug, Clone, Copy)]
pub(in crate::conversion) struct SearchState {
    pub(in crate::conversion) cost: i64,
    pub(in crate::conversion) node: u32,
    pub(in crate::conversion) parent: u32,
    pub(in crate::conversion) class: PathClass,
    pub(in crate::conversion) depth: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::conversion) struct HeapItem {
    pub(in crate::conversion) estimate: i64,
    pub(in crate::conversion) sequence: u64,
    pub(in crate::conversion) state: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(in crate::conversion) enum PathClass {
    Neutral,
    Lexical,
    Reading,
    Katakana,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::conversion) struct SearchRun {
    pub(in crate::conversion) terminal: ConversionSearchTerminal,
    pub(in crate::conversion) states_pushed: usize,
    pub(in crate::conversion) incoherent_prefixes_pruned: usize,
}

impl Ord for HeapItem {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .estimate
            .cmp(&self.estimate)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PathClass {
    pub(in crate::conversion) fn extend(self, surface: Surface) -> Option<Self> {
        let next = match surface {
            Surface::Dictionary { .. } | Surface::User(_) => Self::Lexical,
            Surface::Reading => Self::Reading,
            Surface::Katakana => Self::Katakana,
            Surface::Literal(_) | Surface::Generated(_) => Self::Neutral,
        };
        match (self, next) {
            (current, Self::Neutral) => Some(current),
            (Self::Neutral, next) => Some(next),
            (current, next) if current == next => Some(current),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SurfaceKind {
    Reading,
    Katakana,
}

/// Reject a path that splices a fallback spelling into lexical dictionary
/// entries. A partial match is not evidence that the adjacent unmatched kana
/// belongs to the same word: otherwise an OOV reading such as `ぷろんふと`
/// produces candidates like `プロん富と`. A synthetic fallback must also use
/// one spelling consistently; partial hiragana/katakana mosaics are equally
/// unhelpful. Keep wholly lexical multiword paths (normal Japanese
/// segmentation) and wholly synthetic reading/katakana fallbacks, so
/// conversion always has a safe, lossless result.
fn candidate_path_is_coherent(nodes: &[Node], path: &[usize]) -> bool {
    let mut has_lexical_edge = false;
    let mut fallback = None;
    for &index in path {
        match nodes[index].surface {
            Surface::Dictionary { .. } | Surface::User(_) => has_lexical_edge = true,
            Surface::Reading => {
                if fallback.is_some_and(|surface| surface != SurfaceKind::Reading) {
                    return false;
                }
                fallback = Some(SurfaceKind::Reading);
            }
            Surface::Katakana => {
                if fallback.is_some_and(|surface| surface != SurfaceKind::Katakana) {
                    return false;
                }
                fallback = Some(SurfaceKind::Katakana);
            }
            Surface::Literal(_) | Surface::Generated(_) => {}
        }
    }
    !(has_lexical_edge && fallback.is_some())
}

impl Converter {
    pub(in crate::conversion) fn build_viterbi_candidate(
        &mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        reading: &str,
        mut node: usize,
    ) -> Result<(), ConversionError> {
        self.path.clear();
        loop {
            self.path.push(node);
            let previous = self.nodes[node].best_previous;
            if previous == NONE {
                break;
            }
            node = previous;
        }
        self.path.reverse();
        let final_node = *self.path.last().ok_or(ConversionError::NoPath)?;
        let cost = self.nodes[final_node]
            .best_cost
            .saturating_add(connection_cost(
                dictionary,
                RightContextId::new(self.nodes[final_node].right_id),
                LeftContextId::new(0),
            ));
        let candidate = make_candidate(
            CandidateMaterialization {
                dictionary,
                user_dictionary,
                reading,
                initial_right_id: self.initial_right_id,
                bridge_reading_boundary: self.cross_commit_reading_boundary,
                nodes: &self.nodes,
                path: &self.path,
                generated: &self.generated,
            },
            cost,
        )?;
        self.candidates.push(candidate);
        Ok(())
    }

    pub(in crate::conversion) fn viterbi_path_is_coherent(
        &mut self,
        mut node: usize,
    ) -> Result<bool, ConversionError> {
        self.path.clear();
        loop {
            self.path.push(node);
            let previous = self.nodes[node].best_previous;
            if previous == NONE {
                break;
            }
            node = previous;
        }
        self.path.reverse();
        (!self.path.is_empty())
            .then(|| candidate_path_is_coherent(&self.nodes, &self.path))
            .ok_or(ConversionError::NoPath)
    }

    pub(in crate::conversion) fn search_n_best(
        &mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        reading: &str,
        wanted: usize,
    ) -> Result<SearchRun, ConversionError> {
        let mut budget_reached = false;
        let mut incoherent_prefixes_pruned = 0usize;
        let mut node = self.starts_at[0];
        while node != NONE {
            let lattice_node = self.nodes[node];
            if lattice_node.suffix_cost != i64::MAX {
                let cost = connection_cost(
                    dictionary,
                    RightContextId::new(self.initial_right_id),
                    LeftContextId::new(lattice_node.left_id),
                )
                .saturating_add(lattice_node.local_cost);
                if !self.push_state(
                    node,
                    cost,
                    NONE_STATE,
                    cost.saturating_add(lattice_node.suffix_cost),
                    PathClass::Neutral
                        .extend(lattice_node.surface)
                        .expect("a first edge always defines a coherent class"),
                    1,
                ) {
                    budget_reached = true;
                }
            }
            node = lattice_node.next_from_start;
        }

        while let Some(item) = self.queue.pop() {
            let state = self.states[item.state as usize];
            let lattice_node = self.nodes[state.node as usize];
            if lattice_node.end == reading.len() {
                self.path.clear();
                let mut state_index = item.state;
                loop {
                    let path_state = self.states[state_index as usize];
                    self.path.push(path_state.node as usize);
                    if path_state.parent == NONE_STATE {
                        break;
                    }
                    state_index = path_state.parent;
                }
                self.path.reverse();
                if !candidate_path_is_coherent(&self.nodes, &self.path) {
                    continue;
                }
                let total = state.cost.saturating_add(connection_cost(
                    dictionary,
                    RightContextId::new(lattice_node.right_id),
                    LeftContextId::new(0),
                ));
                // A later (costlier) path stitching together to more than
                // `MAX_PREEDIT_BYTES` is an unremarkable, expected outcome
                // once enough alternatives exist. Skipping it and continuing
                // the search for a smaller alternative keeps this in the same
                // "degrade gracefully" family as the lattice-node and
                // search-state budgets below.
                let candidate = match make_candidate(
                    CandidateMaterialization {
                        dictionary,
                        user_dictionary,
                        reading,
                        initial_right_id: self.initial_right_id,
                        bridge_reading_boundary: self.cross_commit_reading_boundary,
                        nodes: &self.nodes,
                        path: &self.path,
                        generated: &self.generated,
                    },
                    total,
                ) {
                    Ok(candidate) => candidate,
                    Err(ConversionError::OutputTooLong | ConversionError::TooManySegments) => {
                        continue;
                    }
                    Err(error) => return Err(error),
                };
                if !has_same_surface(&self.candidates, candidate.text()) {
                    self.candidates.push(candidate);
                    if self.candidates.len() >= wanted {
                        return Ok(SearchRun {
                            terminal: ConversionSearchTerminal::CandidateLimitReached,
                            states_pushed: self.states.len(),
                            incoherent_prefixes_pruned,
                        });
                    }
                }
                continue;
            }

            if usize::from(state.depth) >= MAX_SEGMENTS {
                continue;
            }

            let mut next = self.starts_at[lattice_node.end];
            while next != NONE {
                let following = self.nodes[next];
                if following.suffix_cost != i64::MAX {
                    let Some(class) = state.class.extend(following.surface) else {
                        incoherent_prefixes_pruned = incoherent_prefixes_pruned.saturating_add(1);
                        next = following.next_from_start;
                        continue;
                    };
                    let cost = state
                        .cost
                        .saturating_add(connection_cost(
                            dictionary,
                            RightContextId::new(lattice_node.right_id),
                            LeftContextId::new(following.left_id),
                        ))
                        .saturating_add(following.local_cost);
                    let estimate = cost.saturating_add(following.suffix_cost);
                    if !self.push_state(
                        next,
                        cost,
                        item.state,
                        estimate,
                        class,
                        state.depth.saturating_add(1),
                    ) {
                        budget_reached = true;
                    }
                }
                next = following.next_from_start;
            }
        }
        Ok(SearchRun {
            terminal: if budget_reached {
                ConversionSearchTerminal::StateBudgetReached
            } else {
                ConversionSearchTerminal::SearchExhausted
            },
            states_pushed: self.states.len(),
            incoherent_prefixes_pruned,
        })
    }

    pub(in crate::conversion) fn push_state(
        &mut self,
        node: usize,
        cost: i64,
        parent: u32,
        estimate: i64,
        class: PathClass,
        depth: u8,
    ) -> bool {
        if self.states.len() >= self.search_state_budget {
            return false;
        }
        let Ok(node) = u32::try_from(node) else {
            return false;
        };
        let Ok(state) = u32::try_from(self.states.len()) else {
            return false;
        };
        self.states.push(SearchState {
            cost,
            node,
            parent,
            class,
            depth,
        });
        self.queue.push(HeapItem {
            estimate,
            sequence: self.sequence,
            state,
        });
        self.sequence = self.sequence.wrapping_add(1);
        true
    }
}
