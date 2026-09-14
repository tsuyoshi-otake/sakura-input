use sakura_values::{FixedStr, MAX_PREEDIT_BYTES};

use crate::dictionary::{Dictionary, EntryFlags};
use crate::input_repair::{
    allows_system_entry, collect_repair_variants, english_spelling_katakana_reading, RepairKind,
    COMMIT_HISTORY_PENALTY, ENGLISH_KATAKANA_PENALTY, MAX_REPAIR_VARIANTS,
};
use crate::preferences::ConversionMethod;
use crate::user_dictionary::UserDictionary;

use super::super::{
    connection_cost, synthetic_run_cost, ConversionError, ConversionOptions, Converter,
    LeftContextId, RightContextId, Surface, BASE_DICTIONARY_EDGES_PER_READING, COUNTER_FORMS,
    COUNTER_WORD_COST, DEFAULT_NOUN_ID, FALLBACK_WORD_COST, KATAKANA_BASE_COST,
    KATAKANA_COST_PER_CHAR, MAX_DICTIONARY_SURFACES_PER_READING, NONE, RUN_BASE_COST,
    RUN_COST_PER_CHAR,
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
struct CharRun {
    end: usize,
    chars: usize,
}

fn char_run(reading: &str, start: usize) -> CharRun {
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

impl Converter {
    pub(in crate::conversion) fn reset(&mut self, reading_len: usize) {
        self.nodes.clear();
        self.states.clear();
        self.queue.clear();
        self.path.clear();
        self.candidates.clear();
        self.generated.clear();
        self.starts_at[..=reading_len].fill(NONE);
        self.ends_at[..=reading_len].fill(NONE);
        self.sequence = 0;
    }

    pub(in crate::conversion) fn build_lattice(
        &mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        reading: &str,
        options: ConversionOptions,
        commit_repairs: &[FixedStr<MAX_PREEDIT_BYTES>],
    ) -> Result<(), ConversionError> {
        if options.method == ConversionMethod::SingleSegment {
            return self.build_single_segment_lattice(
                dictionary,
                user_dictionary,
                reading,
                options,
                commit_repairs,
            );
        }
        let synthetic_id = if dictionary.class_count() > usize::from(DEFAULT_NOUN_ID) {
            DEFAULT_NOUN_ID
        } else {
            0
        };
        let mut previous_class = None;
        // One slot-owned budget for the whole lattice, reset per reading
        // start: a fresh one per start cost a 1,040-byte stack construction
        // per character before #94/#95, and a heap one would allocate.
        for (start, character) in reading.char_indices() {
            let class = char_class(character);
            let run = char_run(reading, start);
            // A run of Latin letters or ASCII digits is one token the user
            // typed, not a phrase to be segmented. Splitting `24` into `2` +
            // `4` lets a superscript `²` steal the first digit and produce
            // `²4日`. Entries that start at the token and reach at least its
            // end remain available.
            let atomic_token = matches!(class, CharClass::AsciiLetter | CharClass::AsciiDigit);
            let at_token_start = previous_class != Some(class) || !atomic_token;
            previous_class = Some(class);
            let mut last_length = 0usize;
            let mut candidates_for_length = 0usize;
            self.edge_budget.reset();
            let mut failure = None;
            dictionary
                .common_prefix_search(&reading[start..], |matched| {
                    if atomic_token && (!at_token_start || start + matched.matched_bytes < run.end)
                    {
                        return true;
                    }
                    if !allows_system_entry(
                        options.input_support,
                        options.skip_input_repair,
                        matched.entry.flags,
                    ) || (start == 0 && matched.entry.flags.contains(EntryFlags::NON_INITIAL))
                    {
                        return true;
                    }
                    if matched.matched_bytes != last_length {
                        last_length = matched.matched_bytes;
                        candidates_for_length = 0;
                        self.edge_budget.reset();
                    }
                    if !self.edge_budget.admit(matched.entry.surface_id) {
                        return true;
                    }
                    candidates_for_length += 1;
                    let boost = if matched.entry.flags.contains(EntryFlags::IT) {
                        let proportional = i64::from(matched.entry.word_cost.max(0))
                            .saturating_mul(i64::from(options.it_bias_per_mille))
                            / 1_000;
                        proportional.min(i64::from(options.max_it_boost))
                    } else {
                        0
                    };
                    let local_cost = i64::from(matched.entry.word_cost).saturating_sub(boost);
                    let Ok(entry_index) = u32::try_from(matched.entry_index) else {
                        return true;
                    };
                    if let Err(error) = self.add_node(
                        dictionary,
                        NodeSpec {
                            start,
                            end: start + matched.matched_bytes,
                            left_id: matched.entry.left_id,
                            right_id: matched.entry.right_id,
                            local_cost,
                            surface: Surface::Dictionary {
                                entry: matched.entry,
                                entry_index,
                                repair: None,
                            },
                        },
                    ) {
                        failure = Some(error);
                        return false;
                    }
                    true
                })
                .map_err(ConversionError::Dictionary)?;
            if let Some(error) = failure {
                return Err(error);
            }

            // Repair edges are best-effort: a full lattice must not fail the
            // conversion when only a typo-correction path ran out of room.
            let _ = self.add_local_repair_edges(
                dictionary,
                &reading[start..],
                start,
                candidates_for_length,
                options,
            );

            if let Some(user_dictionary) = user_dictionary {
                let mut user_candidates_for_length = 0usize;
                let mut last_user_length = 0usize;
                let mut user_failure = None;
                user_dictionary.common_prefix_search(
                    &reading[start..],
                    |matched_bytes, entry_index| {
                        if atomic_token && (!at_token_start || start + matched_bytes < run.end) {
                            return true;
                        }
                        if matched_bytes != last_user_length {
                            last_user_length = matched_bytes;
                            user_candidates_for_length = 0;
                        }
                        if user_candidates_for_length >= BASE_DICTIONARY_EDGES_PER_READING {
                            return true;
                        }
                        user_candidates_for_length += 1;
                        let Some(entry) = user_dictionary.entry(entry_index) else {
                            return true;
                        };
                        let left_id = if usize::from(entry.left_id()) < dictionary.class_count() {
                            entry.left_id()
                        } else {
                            0
                        };
                        let right_id = if usize::from(entry.right_id()) < dictionary.class_count() {
                            entry.right_id()
                        } else {
                            0
                        };
                        if let Err(error) = self.add_node(
                            dictionary,
                            NodeSpec {
                                start,
                                end: start + matched_bytes,
                                left_id,
                                right_id,
                                local_cost: i64::from(entry.word_cost()),
                                surface: Surface::User(entry_index),
                            },
                        ) {
                            user_failure = Some(error);
                            return false;
                        }
                        true
                    },
                );
                if let Some(error) = user_failure {
                    return Err(error);
                }
            }

            let character_end = start + character.len_utf8();
            // Keep ASCII letter/digit runs atomic: a per-character fallback
            // for `2` inside `24` is how a superscript numeral can splice in.
            if !(atomic_token && run.end > character_end) {
                self.add_node(
                    dictionary,
                    NodeSpec {
                        start,
                        end: character_end,
                        left_id: synthetic_id,
                        right_id: synthetic_id,
                        local_cost: FALLBACK_WORD_COST,
                        surface: Surface::Reading,
                    },
                )?;
            }

            if run.end > character_end {
                self.add_node(
                    dictionary,
                    NodeSpec {
                        start,
                        end: run.end,
                        left_id: synthetic_id,
                        right_id: synthetic_id,
                        local_cost: synthetic_run_cost(RUN_BASE_COST, RUN_COST_PER_CHAR, run.chars),
                        surface: Surface::Reading,
                    },
                )?;
            }
            if char_class(character) == CharClass::Hiragana && run.end > character_end {
                self.add_node(
                    dictionary,
                    NodeSpec {
                        start,
                        end: run.end,
                        left_id: synthetic_id,
                        right_id: synthetic_id,
                        local_cost: synthetic_run_cost(
                            KATAKANA_BASE_COST,
                            KATAKANA_COST_PER_CHAR,
                            run.chars,
                        ),
                        surface: Surface::Katakana,
                    },
                )?;
            }
            for (counter_reading, surface) in COUNTER_FORMS {
                if reading[start..].starts_with(counter_reading) {
                    self.add_node(
                        dictionary,
                        NodeSpec {
                            start,
                            end: start + counter_reading.len(),
                            left_id: synthetic_id,
                            right_id: synthetic_id,
                            local_cost: COUNTER_WORD_COST,
                            surface: Surface::Literal(surface),
                        },
                    )?;
                }
            }
            self.add_numeric_forms(dictionary, reading, start, synthetic_id)?;
        }
        // Commit-history repairs are whole-query hints only. Keep them outside
        // the per-character start loop so a full typed match cannot be pasted
        // onto an intermediate suffix span (Issue #63).
        let _ = self.add_commit_repair_edges_for_whole_query(
            dictionary,
            reading,
            options,
            commit_repairs,
        );
        Ok(())
    }

    /// Adds bounded typo-repair and English-spelling edges for one typed span.
    /// Exact dictionary edges keep priority: repair only fills unused slots and
    /// always carries an explicit penalty. Lattice exhaustion drops repair edges
    /// without failing the conversion. Commit-history hints are handled by
    /// [`Self::add_commit_repair_edges_for_whole_query`].
    pub(in crate::conversion) fn add_local_repair_edges(
        &mut self,
        dictionary: &Dictionary<'_>,
        typed_suffix: &str,
        start: usize,
        exact_edges_for_length: usize,
        options: ConversionOptions,
    ) -> Result<(), ConversionError> {
        let mut support = options.input_support;
        if !support.is_active() || options.skip_input_repair || typed_suffix.is_empty() {
            return Ok(());
        }
        let mut remaining_slots =
            BASE_DICTIONARY_EDGES_PER_READING.saturating_sub(exact_edges_for_length);
        if remaining_slots == 0 {
            return Ok(());
        }

        // Reading-only edit-1 variants are prediction-only until a raw-input
        // provenance path can prove the user's intended key sequence. Keep
        // the shared generator available to prediction, but do not even
        // generate Advanced variants on the ordinary conversion path.
        support.advanced = false;
        let variants = collect_repair_variants(typed_suffix, support, MAX_REPAIR_VARIANTS);
        for variant in variants.iter() {
            if remaining_slots == 0 {
                break;
            }
            let added = self.add_repaired_dictionary_edges(
                dictionary,
                variant.repaired.as_str(),
                start,
                start + usize::from(variant.typed_end).min(typed_suffix.len()),
                variant.penalty,
                remaining_slots,
                options,
                variant.kind,
            )?;
            remaining_slots = remaining_slots.saturating_sub(added);
        }

        if support.english_to_katakana {
            if let Some(katakana) = english_spelling_katakana_reading(typed_suffix) {
                let _ = self.add_repaired_dictionary_edges(
                    dictionary,
                    katakana.as_str(),
                    start,
                    start + typed_suffix.len(),
                    ENGLISH_KATAKANA_PENALTY,
                    remaining_slots,
                    options,
                    RepairKind::EnglishSpelling,
                )?;
            }
        }
        Ok(())
    }

    /// Adds commit-history repair edges for the full query only.
    ///
    /// Invariant (Issue #63): each accepted edge has `start == 0`,
    /// `end == reading.len()`, and was collected for this exact typed reading.
    pub(in crate::conversion) fn add_commit_repair_edges_for_whole_query(
        &mut self,
        dictionary: &Dictionary<'_>,
        reading: &str,
        options: ConversionOptions,
        commit_repairs: &[FixedStr<MAX_PREEDIT_BYTES>],
    ) -> Result<(), ConversionError> {
        let support = options.input_support;
        if !support.is_active()
            || options.skip_input_repair
            || !support.commit_based
            || reading.is_empty()
            || commit_repairs.is_empty()
        {
            return Ok(());
        }
        let mut remaining_slots = BASE_DICTIONARY_EDGES_PER_READING;
        for repaired in commit_repairs {
            if remaining_slots == 0 {
                break;
            }
            if repaired.as_str() == reading {
                continue;
            }
            let added = self.add_repaired_dictionary_edges(
                dictionary,
                repaired.as_str(),
                0,
                reading.len(),
                COMMIT_HISTORY_PENALTY,
                remaining_slots,
                options,
                RepairKind::CommitHistory,
            )?;
            remaining_slots = remaining_slots.saturating_sub(added);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub(in crate::conversion) fn add_repaired_dictionary_edges(
        &mut self,
        dictionary: &Dictionary<'_>,
        repaired: &str,
        start: usize,
        typed_end: usize,
        penalty: i64,
        remaining_slots: usize,
        options: ConversionOptions,
        repair: RepairKind,
    ) -> Result<usize, ConversionError> {
        if remaining_slots == 0 || repaired.is_empty() {
            return Ok(0);
        }
        let mut added = 0usize;
        let mut lattice_full = false;
        let result = dictionary.common_prefix_search(repaired, |matched| {
            if matched.matched_bytes != repaired.len() {
                return true;
            }
            if !allows_system_entry(
                options.input_support,
                options.skip_input_repair,
                matched.entry.flags,
            ) || (start == 0 && matched.entry.flags.contains(EntryFlags::NON_INITIAL))
            {
                return true;
            }
            if added >= remaining_slots {
                return false;
            }
            let boost = if matched.entry.flags.contains(EntryFlags::IT) {
                let proportional = i64::from(matched.entry.word_cost.max(0))
                    .saturating_mul(i64::from(options.it_bias_per_mille))
                    / 1_000;
                proportional.min(i64::from(options.max_it_boost))
            } else {
                0
            };
            let local_cost = i64::from(matched.entry.word_cost)
                .saturating_sub(boost)
                .saturating_add(penalty);
            let Ok(entry_index) = u32::try_from(matched.entry_index) else {
                return true;
            };
            match self.add_node(
                dictionary,
                NodeSpec {
                    start,
                    end: typed_end,
                    left_id: matched.entry.left_id,
                    right_id: matched.entry.right_id,
                    local_cost,
                    surface: Surface::Dictionary {
                        entry: matched.entry,
                        entry_index,
                        repair: Some(repair),
                    },
                },
            ) {
                Ok(()) => {
                    added += 1;
                    true
                }
                Err(ConversionError::LatticeFull) => {
                    lattice_full = true;
                    false
                }
                Err(_) => false,
            }
        });
        if lattice_full {
            return Ok(added);
        }
        result.map_err(ConversionError::Dictionary)?;
        Ok(added)
    }

    /// Builds the intentionally narrow single-bunsetsu lattice. Restricting
    /// nodes at construction time keeps N-best, learning metadata, and the
    /// candidate UI honest: no multi-segment path is generated and later
    /// hidden merely for presentation.
    pub(in crate::conversion) fn build_single_segment_lattice(
        &mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        reading: &str,
        options: ConversionOptions,
        commit_repairs: &[FixedStr<MAX_PREEDIT_BYTES>],
    ) -> Result<(), ConversionError> {
        let reading_len = reading.len();
        let synthetic_id = if dictionary.class_count() > usize::from(DEFAULT_NOUN_ID) {
            DEFAULT_NOUN_ID
        } else {
            0
        };
        let mut failure = None;
        let mut exact_edges_added = 0usize;
        self.edge_budget.reset();
        dictionary
            .common_prefix_search(reading, |matched| {
                if matched.matched_bytes != reading_len {
                    return true;
                }
                if !allows_system_entry(
                    options.input_support,
                    options.skip_input_repair,
                    matched.entry.flags,
                ) || matched.entry.flags.contains(EntryFlags::NON_INITIAL)
                {
                    return true;
                }
                if !self.edge_budget.admit(matched.entry.surface_id) {
                    return true;
                }
                let boost = if matched.entry.flags.contains(EntryFlags::IT) {
                    let proportional = i64::from(matched.entry.word_cost.max(0))
                        .saturating_mul(i64::from(options.it_bias_per_mille))
                        / 1_000;
                    proportional.min(i64::from(options.max_it_boost))
                } else {
                    0
                };
                let local_cost = i64::from(matched.entry.word_cost).saturating_sub(boost);
                let Ok(entry_index) = u32::try_from(matched.entry_index) else {
                    return true;
                };
                if let Err(error) = self.add_node(
                    dictionary,
                    NodeSpec {
                        start: 0,
                        end: reading_len,
                        left_id: matched.entry.left_id,
                        right_id: matched.entry.right_id,
                        local_cost,
                        surface: Surface::Dictionary {
                            entry: matched.entry,
                            entry_index,
                            repair: None,
                        },
                    },
                ) {
                    failure = Some(error);
                    return false;
                }
                exact_edges_added = exact_edges_added.saturating_add(1);
                true
            })
            .map_err(ConversionError::Dictionary)?;
        if let Some(error) = failure {
            return Err(error);
        }
        let _ = self.add_local_repair_edges(dictionary, reading, 0, exact_edges_added, options);
        let _ = self.add_commit_repair_edges_for_whole_query(
            dictionary,
            reading,
            options,
            commit_repairs,
        );

        if let Some(user_dictionary) = user_dictionary {
            let mut user_failure = None;
            user_dictionary.common_prefix_search(reading, |matched_bytes, entry_index| {
                if matched_bytes != reading_len {
                    return true;
                }
                let Some(entry) = user_dictionary.entry(entry_index) else {
                    return true;
                };
                let left_id = if usize::from(entry.left_id()) < dictionary.class_count() {
                    entry.left_id()
                } else {
                    0
                };
                let right_id = if usize::from(entry.right_id()) < dictionary.class_count() {
                    entry.right_id()
                } else {
                    0
                };
                if let Err(error) = self.add_node(
                    dictionary,
                    NodeSpec {
                        start: 0,
                        end: reading_len,
                        left_id,
                        right_id,
                        local_cost: i64::from(entry.word_cost()),
                        surface: Surface::User(entry_index),
                    },
                ) {
                    user_failure = Some(error);
                    return false;
                }
                true
            });
            if let Some(error) = user_failure {
                return Err(error);
            }
        }

        let chars = reading.chars().count();
        self.add_node(
            dictionary,
            NodeSpec {
                start: 0,
                end: reading_len,
                left_id: synthetic_id,
                right_id: synthetic_id,
                local_cost: synthetic_run_cost(RUN_BASE_COST, RUN_COST_PER_CHAR, chars),
                surface: Surface::Reading,
            },
        )?;
        if reading
            .chars()
            .all(|character| char_class(character) == CharClass::Hiragana)
        {
            self.add_node(
                dictionary,
                NodeSpec {
                    start: 0,
                    end: reading_len,
                    left_id: synthetic_id,
                    right_id: synthetic_id,
                    local_cost: synthetic_run_cost(
                        KATAKANA_BASE_COST,
                        KATAKANA_COST_PER_CHAR,
                        chars,
                    ),
                    surface: Surface::Katakana,
                },
            )?;
        }
        for (counter_reading, surface) in COUNTER_FORMS {
            if counter_reading == reading {
                self.add_node(
                    dictionary,
                    NodeSpec {
                        start: 0,
                        end: reading_len,
                        left_id: synthetic_id,
                        right_id: synthetic_id,
                        local_cost: COUNTER_WORD_COST,
                        surface: Surface::Literal(surface),
                    },
                )?;
            }
        }
        self.add_numeric_forms(dictionary, reading, 0, synthetic_id)?;
        Ok(())
    }

    pub(in crate::conversion) fn add_node(
        &mut self,
        dictionary: &Dictionary<'_>,
        spec: NodeSpec,
    ) -> Result<(), ConversionError> {
        if self.nodes.len() >= self.lattice_node_budget {
            return Err(ConversionError::LatticeFull);
        }
        let (best_cost, best_previous) = if spec.start == 0 {
            (
                connection_cost(
                    dictionary,
                    RightContextId::new(self.initial_right_id),
                    LeftContextId::new(spec.left_id),
                )
                .saturating_add(spec.local_cost),
                NONE,
            )
        } else {
            let mut previous = self.ends_at[spec.start];
            let mut best_cost = i64::MAX;
            let mut best_previous = NONE;
            while previous != NONE {
                let prior = self.nodes[previous];
                let cost = prior
                    .best_cost
                    .saturating_add(connection_cost(
                        dictionary,
                        RightContextId::new(prior.right_id),
                        LeftContextId::new(spec.left_id),
                    ))
                    .saturating_add(spec.local_cost);
                if cost < best_cost {
                    best_cost = cost;
                    best_previous = previous;
                }
                previous = prior.next_at_end;
            }
            if best_previous == NONE {
                return Ok(());
            }
            (best_cost, best_previous)
        };
        let index = self.nodes.len();
        self.nodes.push(Node {
            start: spec.start,
            end: spec.end,
            left_id: spec.left_id,
            right_id: spec.right_id,
            local_cost: spec.local_cost,
            best_cost,
            best_previous,
            suffix_cost: i64::MAX,
            next_from_start: self.starts_at[spec.start],
            next_at_end: self.ends_at[spec.end],
            surface: spec.surface,
        });
        self.starts_at[spec.start] = index;
        self.ends_at[spec.end] = index;
        Ok(())
    }

    pub(in crate::conversion) fn compute_suffix_costs(
        &mut self,
        dictionary: &Dictionary<'_>,
        reading_len: usize,
    ) {
        for index in (0..self.nodes.len()).rev() {
            let node = self.nodes[index];
            self.nodes[index].suffix_cost = if node.end == reading_len {
                connection_cost(
                    dictionary,
                    RightContextId::new(node.right_id),
                    LeftContextId::new(0),
                )
            } else {
                let mut next = self.starts_at[node.end];
                let mut best = i64::MAX;
                while next != NONE {
                    let following = self.nodes[next];
                    if following.suffix_cost != i64::MAX {
                        let cost = connection_cost(
                            dictionary,
                            RightContextId::new(node.right_id),
                            LeftContextId::new(following.left_id),
                        )
                        .saturating_add(following.local_cost)
                        .saturating_add(following.suffix_cost);
                        best = best.min(cost);
                    }
                    next = following.next_from_start;
                }
                best
            };
        }
    }

    pub(in crate::conversion) fn best_final_node(
        &self,
        dictionary: &Dictionary<'_>,
        reading_len: usize,
    ) -> Result<usize, ConversionError> {
        let mut current = self.ends_at[reading_len];
        let mut best = NONE;
        let mut best_cost = i64::MAX;
        while current != NONE {
            let node = self.nodes[current];
            let cost = node.best_cost.saturating_add(connection_cost(
                dictionary,
                RightContextId::new(node.right_id),
                LeftContextId::new(0),
            ));
            if cost < best_cost {
                best = current;
                best_cost = cost;
            }
            current = node.next_at_end;
        }
        (best != NONE)
            .then_some(best)
            .ok_or(ConversionError::NoPath)
    }
}
