//! Offline replay of correction learning (#305), compiled only with the
//! `replay-eval` feature for `learning-replay` in `tools/conversion-eval`.
//!
//! The replay drives the engine's own correction trail, staged learning,
//! ranking with tombstones, and first-candidate choice, so the tool measures
//! the code that ships rather than a copy of it. What the history cannot show
//! (the conversion cache, the reranker, host-side caret moves) is left out.
//! The shipping engine never enables this feature.

use sakura_core::conversion::CandidateOrigin;
use sakura_core::ConversionCandidate;

use crate::correction::{Correction, CorrectionTrail};
use crate::dispatch::{candidate_learning_key, preferred_candidate_index};
use crate::learning::{LearningService, StagedCorrection};

/// One session's correction trail, fed with what the history recorded.
#[derive(Debug, Default)]
pub struct ReplayTrail(CorrectionTrail);

/// A commit that retyped an erased commit's reading with another surface.
#[derive(Debug)]
pub struct ReplayCorrection(Correction);

impl ReplayCorrection {
    /// The left context the first erased commit was made in.
    pub fn origin_left(&self) -> u16 {
        self.0.origin_left()
    }

    /// The erased surfaces this commit replaced.
    pub fn rejected(&self) -> impl Iterator<Item = &str> {
        self.0.rejected().iter().map(|rejected| rejected.surface())
    }

    fn staged(&self) -> StagedCorrection {
        StagedCorrection::new(
            self.0.origin_left(),
            self.0
                .rejected()
                .iter()
                .map(|rejected| (rejected.surface(), rejected.right_context())),
        )
    }
}

/// What one commit did to the trail.
#[derive(Debug)]
pub struct ReplayRetype {
    /// The commit retyped the reading of the erased commit next to the caret.
    pub retyped: bool,
    /// The retype changed the surface: a correction.
    pub correction: Option<ReplayCorrection>,
}

impl ReplayTrail {
    /// A persisted commit; see `CorrectionTrail::commit`.
    pub fn commit(&mut self, commit: ReplayCommit<'_>) -> ReplayRetype {
        let retyped = self.0.would_retype(commit.reading);
        let correction = self
            .0
            .commit(
                commit.reading,
                commit.surface,
                commit.left_context,
                commit.right_context,
            )
            .map(ReplayCorrection);
        ReplayRetype {
            retyped,
            correction,
        }
    }

    /// An idle plain or Shift Backspace the host handled.
    pub fn erase(&mut self) {
        self.0.erase();
    }

    /// Any other idle key, or anything that moved the caret's context.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// The host deleted the undone commit `surface` whole.
    pub fn undo_applied(&mut self, surface: &str) {
        self.0.undo_applied(surface);
    }
}

/// The first candidate the engine would select, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayChoice {
    /// Index into the candidates passed to [`ReplayLearning::first_candidate`].
    pub index: usize,
    /// An exact-context preference selected it.
    pub exact: bool,
    /// Any learned preference (exact or general) selected it.
    pub learned: bool,
}

/// An in-memory learning service whose clock the replay sets.
#[derive(Debug)]
pub struct ReplayLearning {
    service: LearningService,
    /// Corrections are ignored, which is the engine before #305.
    corrections: bool,
}

impl ReplayLearning {
    /// `corrections` false replays the engine before #305: no correction is
    /// recorded, and a commit that only corrected is not staged.
    pub fn new(corrections: bool) -> Self {
        Self {
            service: LearningService::memory(),
            corrections,
        }
    }

    /// Moves the replay to `day` (days since the Unix epoch, nonzero).
    pub fn set_day(&self, day: u32) {
        self.service.set_replay_day(day.max(1));
    }

    /// Learns one exported record directly, as replaying the log would.
    pub fn seed(&self, reading: &str, surface: &str, left_context: u16, right_context: u16) {
        self.service
            .learn(reading, surface, left_context, right_context);
    }

    /// Applies one exported tombstone directly, as replaying the log would.
    /// Ignored when replaying the engine before #305.
    pub fn seed_rejection(
        &self,
        reading: &str,
        surface: &str,
        left_context: u16,
        right_context: u16,
    ) {
        if self.corrections {
            self.service
                .replay_rejection(reading, surface, left_context, right_context);
        }
    }

    /// Stages a commit. `learnable` is false for commits that teach nothing
    /// (a kana transform, a raw repair), as `record_learning` decides.
    pub fn stage_commit(
        &self,
        owner: u64,
        commit: ReplayCommit<'_>,
        learnable: bool,
        correction: Option<&ReplayCorrection>,
    ) {
        let correction = correction.filter(|_| self.corrections);
        if learnable {
            self.service.stage(
                owner,
                commit.reading,
                commit.surface,
                commit.left_context,
                commit.right_context,
                correction.map(ReplayCorrection::staged),
            );
        } else if let Some(correction) = correction {
            self.service.stage_reject_only(
                owner,
                commit.reading,
                commit.surface,
                commit.left_context,
                commit.right_context,
                correction.staged(),
            );
        } else {
            self.service.confirm_staged(owner);
        }
    }

    /// The owner's next action kept the staged commit.
    pub fn confirm_staged(&self, owner: u64) {
        self.service.confirm_staged(owner);
    }

    /// The owner erased or undid the staged commit.
    pub fn discard_staged(&self, owner: u64) {
        self.service.discard_staged(owner);
    }

    /// A conversion is about to rank candidates.
    pub fn confirm_all_staged(&self) {
        self.service.confirm_all_staged();
    }

    /// The candidate a fresh conversion of `reading` after `left_context`
    /// selects first, without the conversion cache or the reranker.
    pub fn first_candidate(
        &self,
        reading: &str,
        left_context: u16,
        candidates: &[ConversionCandidate],
    ) -> ReplayChoice {
        let direct_limit = candidates
            .iter()
            .position(|candidate| candidate.origin() != CandidateOrigin::Direct)
            .unwrap_or(candidates.len());
        let learned = self.service.preference(
            reading,
            left_context,
            candidates[..direct_limit]
                .iter()
                .map(candidate_learning_key),
        );
        let index = preferred_candidate_index(candidates, reading, 0, None, learned, direct_limit);
        let exact = learned.exact == Some(index);
        ReplayChoice {
            index,
            exact,
            learned: exact || learned.general == Some(index),
        }
    }
}

/// One persisted commit as the input history recorded it.
#[derive(Debug, Clone, Copy)]
pub struct ReplayCommit<'a> {
    pub reading: &'a str,
    pub surface: &'a str,
    pub left_context: u16,
    pub right_context: u16,
}
