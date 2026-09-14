use crate::dictionary::Dictionary;
use crate::user_dictionary::UserDictionary;

use super::{
    has_same_surface, CandidateOrigin, ConversionCandidate, ConversionDiagnostics, ConversionError,
    ConversionInput, ConversionOptions, ConversionResult, ConversionSearchTerminal, Converter,
    RawRepairPlan, RepairTier, MAX_CONVERSION_CANDIDATES, MAX_LATTICE_NODES, MAX_RAW_REPAIR_PLANS,
    MAX_SEARCH_STATES,
};

impl Converter {
    /// Runs an original direct conversion and a bounded sequence of corrected
    /// readings while retaining this one [`Converter`] arena.  The corrected
    /// passes intentionally use only the system dictionary and set
    /// `skip_input_repair`; they can therefore never recursively acquire a
    /// conversion slot or feed another repair pass.
    ///
    /// Direct candidates are copied to slot-owned heap scratch before the
    /// first corrected pass. They retain their original order and authority.
    /// When they already fill the public cap, one admitted local repair may
    /// replace only the lowest-priority evictable direct tail; candidate zero,
    /// exact literals, and user-backed candidates are never displaced.
    pub fn convert_input_with_raw_repair_plans<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        original_input: ConversionInput<'_>,
        plans: &[RawRepairPlan],
        options: ConversionOptions,
    ) -> Result<ConversionResult<'a>, ConversionError> {
        // The direct pass consumes the one-shot civil date and commit-history
        // hints. Corrected passes must not recreate either piece of request
        // state while they reuse the converter arena.
        let direct_diagnostics = {
            let result = self.convert_with_user_dictionary_input_detailed(
                dictionary,
                user_dictionary,
                original_input,
                options,
            )?;
            result.diagnostics()
        };
        self.raw_direct_scratch.clear();
        self.raw_direct_scratch
            .extend(self.candidates.iter().cloned());
        self.raw_repair_scratch.clear();

        let mut diagnostics = direct_diagnostics;
        let direct_fills_cap = self.raw_direct_scratch.len() >= options.max_candidates;
        let direct_eviction_index = direct_fills_cap.then(|| {
            self.raw_direct_scratch
                .iter()
                .enumerate()
                .skip(1)
                .rfind(|(_, candidate)| {
                    !candidate.is_synthetic_exact() && candidate.path_evidence().user_edges == 0
                })
                .map(|(index, _)| index)
        });
        if plans.is_empty() || (direct_fills_cap && direct_eviction_index.flatten().is_none()) {
            return Ok(ConversionResult {
                candidates: &self.candidates,
                diagnostics,
            });
        }
        let direct_eviction_index = direct_eviction_index.flatten();

        let budget = options.raw_repair_budget;
        let max_passes = budget.max_corrected_passes.min(MAX_RAW_REPAIR_PLANS);
        let max_repair_candidates = budget.max_repair_candidates.min(MAX_CONVERSION_CANDIDATES);
        let repair_slot_limit = if direct_fills_cap {
            1
        } else {
            options
                .max_candidates
                .saturating_sub(self.raw_direct_scratch.len())
        }
        .min(max_repair_candidates);
        let max_lattice_nodes = budget.max_lattice_nodes.min(MAX_LATTICE_NODES);
        let max_search_states = budget.max_search_states.min(MAX_SEARCH_STATES);
        let mut passes = 0usize;
        let mut aggregate_candidates_examined = 0usize;
        let mut rejected = 0usize;
        let mut aggregate_lattice_nodes = 0usize;
        let mut aggregate_search_states = 0usize;
        let plan_count = plans.len().min(MAX_RAW_REPAIR_PLANS);
        for (plan_index, plan) in plans.iter().take(MAX_RAW_REPAIR_PLANS).enumerate() {
            if repair_slot_limit == 0 {
                break;
            }
            if passes >= max_passes
                || aggregate_candidates_examined >= max_repair_candidates
                || (!direct_fills_cap && self.raw_repair_scratch.len() >= repair_slot_limit)
            {
                break;
            }
            // Phase 1 is structural local completion only.  Keep the general
            // insertion tier representable for the later experiment, but do
            // not run or admit it from this production-facing API.
            if plan.tier() != RepairTier::LocalCompletion {
                rejected = rejected.saturating_add(1);
                continue;
            }
            if !plan.is_valid_for(original_input.lookup_reading)
                || plan
                    .map
                    .validate_for_readings(original_input.lookup_reading, plan.corrected_reading())
                    .is_err()
            {
                rejected = rejected.saturating_add(1);
                continue;
            }

            if aggregate_lattice_nodes >= max_lattice_nodes
                || aggregate_search_states >= max_search_states
            {
                rejected = rejected.saturating_add(1);
                break;
            }
            let remaining_repair_candidates =
                max_repair_candidates.saturating_sub(aggregate_candidates_examined);
            if remaining_repair_candidates == 0 {
                break;
            }
            let mut corrected_options = options;
            // Divide the remaining repair output fairly across the remaining
            // bounded local plans.  A first spelling can otherwise consume
            // all 18 dictionary rows and prevent a later key (for example
            // `e` in nazka) from ever being examined.  With one plan this is
            // the old full breadth; with a full direct list the reservation
            // naturally yields one row per plan.
            let plans_remaining = plan_count.saturating_sub(plan_index).max(1);
            let remaining_output_slots =
                repair_slot_limit.saturating_sub(self.raw_repair_scratch.len());
            let fair_share = remaining_output_slots.div_ceil(plans_remaining);
            corrected_options.max_candidates = fair_share.min(remaining_repair_candidates).max(1);
            corrected_options.skip_input_repair = true;
            // Do not pass a user dictionary to a Phase 1 corrected pass.  A
            // user edge would make the lexical evidence non-admissible.
            let previous_lattice_budget = self.lattice_node_budget;
            let previous_search_budget = self.search_state_budget;
            self.lattice_node_budget = previous_lattice_budget
                .min(max_lattice_nodes.saturating_sub(aggregate_lattice_nodes));
            self.search_state_budget = previous_search_budget
                .min(max_search_states.saturating_sub(aggregate_search_states));
            passes = passes.saturating_add(1);
            let pass = match self.convert_with_user_dictionary_input_detailed(
                dictionary,
                None,
                ConversionInput::ordinary(plan.corrected_reading()),
                corrected_options,
            ) {
                Ok(result) => result.diagnostics(),
                Err(_) => {
                    aggregate_lattice_nodes =
                        aggregate_lattice_nodes.saturating_add(self.nodes.len());
                    aggregate_search_states =
                        aggregate_search_states.saturating_add(self.states.len());
                    aggregate_candidates_examined =
                        aggregate_candidates_examined.saturating_add(self.candidates.len());
                    self.lattice_node_budget = previous_lattice_budget;
                    self.search_state_budget = previous_search_budget;
                    rejected = rejected.saturating_add(1);
                    continue;
                }
            };
            aggregate_lattice_nodes = aggregate_lattice_nodes.saturating_add(pass.lattice_nodes);
            aggregate_search_states = aggregate_search_states.saturating_add(pass.states_pushed);
            aggregate_candidates_examined =
                aggregate_candidates_examined.saturating_add(self.candidates.len());
            self.lattice_node_budget = previous_lattice_budget;
            self.search_state_budget = previous_search_budget;
            if !matches!(
                pass.terminal,
                ConversionSearchTerminal::SearchExhausted
                    | ConversionSearchTerminal::CandidateLimitReached
            ) {
                rejected = rejected.saturating_add(1);
                continue;
            }

            rejected = rejected.saturating_add(self.admit_repair_pass(
                plan,
                repair_slot_limit,
                direct_fills_cap,
            ));
        }

        self.candidates.clear();
        self.candidates
            .extend(self.raw_direct_scratch.iter().cloned());
        if !self.raw_repair_scratch.is_empty() {
            if let Some(index) = direct_eviction_index {
                self.candidates.remove(index);
            }
        }
        let repair_candidates_added = self.merge_repair_scratch(options.max_candidates);
        diagnostics.raw_repair_passes = passes;
        diagnostics.raw_repair_candidates_added = repair_candidates_added;
        diagnostics.raw_repair_candidates_examined = aggregate_candidates_examined;
        diagnostics.raw_repair_candidates_rejected = rejected;
        diagnostics.raw_repair_lattice_nodes = aggregate_lattice_nodes;
        diagnostics.raw_repair_search_states = aggregate_search_states;
        Ok(ConversionResult {
            candidates: &self.candidates,
            diagnostics,
        })
    }

    /// Admits one corrected pass's fully-covered candidates into the repair
    /// scratch and reports how many rows it rejected.
    ///
    /// This is a sibling frame on purpose, not an inlined block.
    /// `ConversionCandidate` is 4,152 bytes, an unoptimised build gives every
    /// by-value move and every temporary its own stack slot, and ten of them
    /// sat in `convert_input_with_raw_repair_plans` — a 41,456-byte frame that
    /// stayed live across the corrected pass's entire conversion subtree. Held
    /// here they are live only while nothing deep is running.
    /// `raw_multi_pass_core_path_fits_128_kib_thread_stack` is what fails when
    /// this moves back inline; keep the orchestrating frame free of candidates.
    #[inline(never)]
    pub(in crate::conversion) fn admit_repair_pass(
        &mut self,
        plan: &RawRepairPlan,
        repair_slot_limit: usize,
        direct_fills_cap: bool,
    ) -> usize {
        let accepted: Vec<ConversionCandidate> = self
            .candidates
            .iter()
            .filter(|candidate| candidate.has_full_system_coverage(plan.corrected_reading().len()))
            .cloned()
            .collect();
        if accepted.is_empty() {
            return 1;
        }
        let mut rejected = 0usize;
        for mut candidate in accepted {
            candidate.origin = CandidateOrigin::RawRepair {
                plan_id: plan.plan_id(),
                tier: plan.tier(),
            };
            if has_same_surface(&self.raw_direct_scratch, candidate.text()) {
                rejected = rejected.saturating_add(1);
                continue;
            }
            if let Some(existing) = self
                .raw_repair_scratch
                .iter_mut()
                .find(|existing| existing.text() == candidate.text())
            {
                if candidate.authority().rank() > existing.authority().rank()
                    || (candidate.authority() == existing.authority()
                        && candidate.cost < existing.cost)
                {
                    *existing = candidate;
                }
                continue;
            }
            if self.raw_repair_scratch.len() < repair_slot_limit {
                self.raw_repair_scratch.push(candidate);
            } else if direct_fills_cap {
                let worst = self
                    .raw_repair_scratch
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, existing)| existing.cost)
                    .map(|(index, _)| index)
                    .expect("a full reservation has a candidate");
                if candidate.cost < self.raw_repair_scratch[worst].cost {
                    self.raw_repair_scratch[worst] = candidate;
                } else {
                    rejected = rejected.saturating_add(1);
                }
            } else {
                rejected = rejected.saturating_add(1);
            }
        }
        rejected
    }

    /// Appends the repair scratch to the finished candidate list, skipping
    /// surfaces the direct pass already produced, and reports how many rows it
    /// added. Split out for the same stack reason as [`Self::admit_repair_pass`].
    #[inline(never)]
    pub(in crate::conversion) fn merge_repair_scratch(&mut self, max_candidates: usize) -> usize {
        let mut added = 0usize;
        for candidate in self.raw_repair_scratch.iter().cloned() {
            if self.candidates.len() >= max_candidates {
                break;
            }
            if has_same_surface(&self.candidates, candidate.text()) {
                continue;
            }
            self.candidates.push(candidate);
            added = added.saturating_add(1);
        }
        added
    }

    /// Backward-compatible ordinary-input wrapper. Classified production
    /// callers should use [`Self::convert_input_with_raw_repair_plans`] so the
    /// direct pass cannot lose an exact literal policy at the repair boundary.
    pub fn convert_with_raw_repair_plans<'a>(
        &'a mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        original_reading: &str,
        plans: &[RawRepairPlan],
        options: ConversionOptions,
    ) -> Result<ConversionResult<'a>, ConversionError> {
        self.convert_input_with_raw_repair_plans(
            dictionary,
            user_dictionary,
            ConversionInput::ordinary(original_reading),
            plans,
            options,
        )
    }

    /// Input-aware closure wrapper for production callers that must consume
    /// the candidate slice before the converter slot is released.
    pub fn with_raw_repair_input_conversion<R>(
        &mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        original_input: ConversionInput<'_>,
        plans: &[RawRepairPlan],
        options: ConversionOptions,
        consume: impl FnOnce(&[ConversionCandidate], ConversionDiagnostics) -> R,
    ) -> Result<R, ConversionError> {
        let result = self.convert_input_with_raw_repair_plans(
            dictionary,
            user_dictionary,
            original_input,
            plans,
            options,
        )?;
        let diagnostics = result.diagnostics();
        Ok(consume(result.candidates(), diagnostics))
    }

    /// Closure-shaped convenience wrapper for callers that must consume the
    /// candidate slice before the converter slot is released.
    pub fn with_raw_repair_conversion<R>(
        &mut self,
        dictionary: &Dictionary<'_>,
        user_dictionary: Option<&UserDictionary>,
        original_reading: &str,
        plans: &[RawRepairPlan],
        options: ConversionOptions,
        consume: impl FnOnce(&[ConversionCandidate], ConversionDiagnostics) -> R,
    ) -> Result<R, ConversionError> {
        let result = self.convert_input_with_raw_repair_plans(
            dictionary,
            user_dictionary,
            ConversionInput::ordinary(original_reading),
            plans,
            options,
        )?;
        let diagnostics = result.diagnostics();
        Ok(consume(result.candidates(), diagnostics))
    }
}
