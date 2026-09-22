//! Process-wide composition activity for idle Space absorption.
//!
//! Engine pipe workers are share-nothing: each connection owns its own
//! [`crate::dispatch::Dispatcher`]. Electron / Cursor can still deliver one
//! physical Space to every live TSF context of the same host process. When
//! one of those contexts is composing or converting, an idle peer must not
//! insert a document space for that same Space key.
//!
//! Counts are keyed by host `process_name` (what CreateSession reports),
//! compared ASCII-case-insensitively. Distinct executables do not fence
//! each other. Same-name processes share a count — a deliberate bound when
//! two Notepad windows both report `notepad.exe`.
//!
//! A claim can also disappear *abnormally*. The DLL gives a keystroke a
//! 50 ms round trip; when that budget expires it drops the pipe, and the
//! composing session is torn down with its reading still live. The next
//! Space then finds an idle host and commits U+3000 into the user's
//! document — the measured #102 symptom.
//! Finalizing an owned claim with `teardown = true` arms a one-shot latch
//! for that host so the Space right after a broken link can be absorbed.
//!
//! The two fences are read through deliberately different queries, and the
//! difference is the whole safety argument:
//!
//! - [`CompositionFence::any_active`] reports *live* claims only. Its
//!   caller suppresses every idle key, which is right when the idle
//!   connection is a duplicate of a composing one.
//! - [`CompositionFence::consume_teardown`] reports a *lost* claim, and
//!   spends it in the same breath. The connection that replaces a dropped
//!   link is the user's real input path, so only the one keystroke that
//!   would have become a document space may be absorbed — never its
//!   letters, and never a second Space.
//!
//! The latch is counted rather than timed. One absorbed Space per teardown
//! covers the failure, can never permanently swallow a full-width space,
//! and holds no wall clock — so the independent oracle in
//! `sakura-oracles::space_key_dispatch_oracle` models it exactly rather than
//! approximately. A composition that ends normally releases
//! its owned claim with `teardown = false` and arms nothing.
//!
//! Pending latches retain the most recently armed 1,024 distinct host names
//! (#259). Rearming refreshes that host; at capacity the oldest pending host
//! loses its latch. No time expiry or extra Space absorption is introduced.
//! Live claims are separate and are never evicted by this history bound.

use sakura_ipc::ConnectionProbe;
use sakura_proto::SessionId;
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, MutexGuard};

const MAX_PENDING_TEARDOWNS: usize = 1_024;

/// Shared across every pipe worker in one engine process.
#[derive(Debug, Default)]
pub struct CompositionFence {
    state: Mutex<FenceState>,
}

#[derive(Debug, Default)]
struct FenceState {
    /// Number of live composing/converting claims per host process name.
    counts: HashMap<Box<str>, u32>,
    /// Hosts that lost a live reading and still owe one-shot recovery,
    /// oldest first; bounded independently of lifetime session count.
    /// Linear operations inspect at most 1,024 short names,
    /// while live-claim lookups remain hash-based.
    torn_down: VecDeque<Box<str>>,
    /// Only claims for the queried host are inspected. Inactive records stay
    /// until their owner finalizes, so late teardown cannot spend them twice.
    owned: HashMap<Box<str>, HashMap<(u64, SessionId), OwnedClaim>>,
}

#[derive(Debug)]
struct OwnedClaim {
    probe: Option<ConnectionProbe>,
    active: bool,
}

impl CompositionFence {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when any connection for `process_name` currently claims an
    /// active composition or conversion.
    ///
    /// Live claims only. A pending teardown is deliberately invisible here:
    /// this answer suppresses *every* idle key, and doing that to the
    /// connection that replaced a dropped link would eat the characters the
    /// user typed into it.
    pub fn any_active(&self, process_name: &str) -> bool {
        let key = normalize_process_name(process_name);
        let mut state = self.lock();
        state.retire_disconnected(key.as_ref());
        state.counts.get(key.as_ref()).copied().unwrap_or_default() > 0
    }

    pub(crate) fn acquire_owned(
        &self,
        process_name: &str,
        owner: u64,
        session: SessionId,
        probe: Option<ConnectionProbe>,
    ) {
        let key = normalize_process_name(process_name);
        let mut state = self.lock();
        state.retire_disconnected(key.as_ref());
        let claims = state.owned.entry(key.clone()).or_default();
        if let Some(existing) = claims.get_mut(&(owner, session)) {
            // Binding the accepted connection to an already staged fixture
            // or session never acquires a second claim or revives a dead one.
            existing.probe = probe;
            return;
        }
        let active = !probe.as_ref().is_some_and(ConnectionProbe::is_disconnected);
        claims.insert((owner, session), OwnedClaim { probe, active });
        if active {
            state.disarm_teardown(key.as_ref());
            *state.counts.entry(key).or_default() += 1;
        }
    }

    pub(crate) fn release_owned(
        &self,
        process_name: &str,
        owner: u64,
        session: SessionId,
        teardown: bool,
    ) {
        let key = normalize_process_name(process_name);
        let mut state = self.lock();
        let Some(claims) = state.owned.get_mut(key.as_ref()) else {
            return;
        };
        let Some(claim) = claims.remove(&(owner, session)) else {
            return;
        };
        if claims.is_empty() {
            state.owned.remove(key.as_ref());
        }
        if claim.active && state.release_count(key.as_ref()) && teardown {
            state.arm_teardown(key);
        }
    }

    /// Spends the absorption a torn-down composition is owed, if one is
    /// owed. `true` when the caller should absorb this keystroke.
    ///
    /// Consuming: asking is deciding. Only the path that will actually
    /// apply the key may call this — a probe that answers what a key
    /// *would* do must not spend the latch a real Space is entitled to.
    pub fn consume_teardown(&self, process_name: &str) -> bool {
        let key = normalize_process_name(process_name);
        let mut state = self.lock();
        state.retire_disconnected(key.as_ref());
        // A live claim already fences this host through `any_active`. Keep
        // the latch for the teardown it was armed for.
        if state.counts.get(key.as_ref()).copied().unwrap_or_default() > 0 {
            return false;
        }
        state.disarm_teardown(key.as_ref())
    }

    fn lock(&self) -> MutexGuard<'_, FenceState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl FenceState {
    fn arm_teardown(&mut self, key: Box<str>) {
        self.disarm_teardown(key.as_ref());
        if self.torn_down.len() == MAX_PENDING_TEARDOWNS {
            // Pop before push so the backing allocation never grows for a
            // transient capacity + 1 entry.
            self.torn_down.pop_front();
        }
        self.torn_down.push_back(key);
    }

    fn disarm_teardown(&mut self, key: &str) -> bool {
        let Some(index) = self.torn_down.iter().position(|name| name.as_ref() == key) else {
            return false;
        };
        self.torn_down.remove(index);
        true
    }

    fn retire_disconnected(&mut self, key: &str) {
        let mut retired = 0;
        if let Some(claims) = self.owned.get_mut(key) {
            for claim in claims.values_mut() {
                if claim.active
                    && claim
                        .probe
                        .as_ref()
                        .is_some_and(ConnectionProbe::is_disconnected)
                {
                    claim.active = false;
                    retired += 1;
                }
            }
        }
        for _ in 0..retired {
            if self.release_count(key) {
                self.arm_teardown(Box::from(key));
            }
        }
    }

    /// Drops one claim. `true` when this host actually held one, which is
    /// what makes an abnormal teardown worth a latch.
    fn release_count(&mut self, key: &str) -> bool {
        let Some(count) = self.counts.get_mut(key) else {
            return false;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            self.counts.remove(key);
        }
        true
    }
}

fn normalize_process_name(process_name: &str) -> Box<str> {
    Box::from(process_name.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_host_names_are_bounded_and_newest_recovery_is_one_shot() {
        let fence = CompositionFence::new();
        for index in 0..10_000 {
            let name = format!("host-{index:05}.exe");
            fence.acquire_owned(&name, 1, 1, None);
            fence.release_owned(&name, 1, 1, true);
        }
        {
            let state = fence.lock();
            assert!(state.counts.is_empty());
            assert!(state.owned.is_empty());
            assert_eq!(state.torn_down.len(), 1_024);
        }
        assert!(!fence.consume_teardown("host-00000.exe"));
        for index in 8_976..10_000 {
            let name = format!("host-{index:05}.exe");
            assert!(fence.consume_teardown(&name));
            assert!(!fence.consume_teardown(&name));
        }
    }

    #[test]
    fn rearming_refreshes_recency_without_banking_spaces_or_growing_storage() {
        let fence = CompositionFence::new();
        for index in 0..MAX_PENDING_TEARDOWNS {
            let name = format!("host-{index:05}.exe");
            fence.acquire_owned(&name, 1, 1, None);
            fence.release_owned(&name, 1, 1, true);
        }
        let capacity = fence.lock().torn_down.capacity();
        fence.acquire_owned("HOST-00000.EXE", 1, 1, None);
        fence.release_owned("host-00000.exe", 1, 1, true);
        fence.acquire_owned("new.exe", 1, 1, None);
        fence.release_owned("new.exe", 1, 1, true);
        assert_eq!(fence.lock().torn_down.capacity(), capacity);
        assert_eq!(fence.lock().torn_down.len(), MAX_PENDING_TEARDOWNS);
        assert!(!fence.consume_teardown("host-00001.exe"));
        assert!(fence.consume_teardown("host-00000.exe"));
        assert!(!fence.consume_teardown("host-00000.exe"));
        assert!(fence.consume_teardown("new.exe"));
    }

    #[test]
    fn a_repeated_teardown_refreshes_a_host_with_a_surviving_claim() {
        let fence = CompositionFence::new();
        for session in 1..=3 {
            fence.acquire_owned("active.exe", 1, session, None);
        }
        fence.release_owned("active.exe", 1, 1, true);
        for index in 1..MAX_PENDING_TEARDOWNS {
            let name = format!("host-{index:05}.exe");
            fence.acquire_owned(&name, 2, 1, None);
            fence.release_owned(&name, 2, 1, true);
        }
        fence.release_owned("active.exe", 1, 2, true);
        fence.acquire_owned("new.exe", 2, 1, None);
        fence.release_owned("new.exe", 2, 1, true);
        assert!(fence.any_active("active.exe"));
        assert!(!fence.consume_teardown("active.exe"));
        assert!(!fence.consume_teardown("host-00001.exe"));
        fence.release_owned("active.exe", 1, 3, false);
        assert!(fence.consume_teardown("active.exe"));
        assert!(!fence.consume_teardown("active.exe"));
    }

    #[test]
    fn saturation_never_evicts_a_live_claim() {
        let fence = CompositionFence::new();
        fence.acquire_owned("active.exe", 1, 1, None);
        for index in 0..MAX_PENDING_TEARDOWNS + 1 {
            let name = format!("host-{index:05}.exe");
            fence.acquire_owned(&name, 2, 1, None);
            fence.release_owned(&name, 2, 1, true);
        }
        assert!(fence.any_active("active.exe"));
        assert!(!fence.consume_teardown("active.exe"));
        fence.release_owned("active.exe", 1, 1, true);
        assert!(!fence.any_active("active.exe"));
        assert!(fence.consume_teardown("active.exe"));
    }

    #[test]
    fn repeated_binding_and_wrong_or_late_finalization_cannot_rearm() {
        let fence = CompositionFence::new();
        fence.acquire_owned("host.exe", 1, 1, None);
        fence.acquire_owned("host.exe", 1, 1, None);
        fence.release_owned("host.exe", 2, 1, true);
        fence.release_owned("host.exe", 1, 2, true);
        assert!(fence.any_active("host.exe"));
        fence.release_owned("host.exe", 1, 1, true);
        assert!(!fence.any_active("host.exe"));
        assert!(fence.consume_teardown("host.exe"));
        fence.release_owned("host.exe", 1, 1, true);
        assert!(!fence.consume_teardown("host.exe"));
        assert!(fence.lock().owned.is_empty());
    }

    #[test]
    fn concurrent_distinct_owners_finalize_under_the_same_history_bound() {
        let fence = CompositionFence::new();
        std::thread::scope(|scope| {
            for owner in 0..4 {
                let fence = &fence;
                scope.spawn(move || {
                    for session in 0..1_000 {
                        let name = format!("host-{owner}-{session}.exe");
                        fence.acquire_owned(&name, owner, session, None);
                        fence.release_owned(&name, owner, session, true);
                    }
                });
            }
        });
        let state = fence.lock();
        assert!(state.counts.is_empty());
        assert!(state.owned.is_empty());
        assert_eq!(state.torn_down.len(), MAX_PENDING_TEARDOWNS);
    }

    #[test]
    fn peer_becomes_active_only_after_acquire() {
        let fence = CompositionFence::new();
        assert!(!fence.any_active("cursor.exe"));
        fence.acquire_owned("cursor.exe", 1, 1, None);
        assert!(fence.any_active("cursor.exe"));
        assert!(!fence.any_active("notepad.exe"));
        fence.release_owned("cursor.exe", 1, 1, false);
        assert!(!fence.any_active("cursor.exe"));
    }

    #[test]
    fn process_name_matching_is_ascii_case_insensitive() {
        let fence = CompositionFence::new();
        fence.acquire_owned("Cursor.exe", 1, 1, None);
        assert!(fence.any_active("cursor.exe"));
        fence.release_owned("CURSOR.EXE", 1, 1, false);
        assert!(!fence.any_active("Cursor.exe"));
        fence.acquire_owned("Cursor.exe", 1, 1, None);
        fence.release_owned("CURSOR.EXE", 1, 1, true);
        assert!(fence.consume_teardown("cursor.exe"));
    }

    #[test]
    fn two_claims_keep_the_host_active_until_both_release() {
        let fence = CompositionFence::new();
        fence.acquire_owned("app.exe", 1, 1, None);
        fence.acquire_owned("app.exe", 2, 1, None);
        fence.release_owned("app.exe", 1, 1, false);
        assert!(fence.any_active("app.exe"));
        fence.release_owned("app.exe", 2, 1, false);
        assert!(!fence.any_active("app.exe"));
    }

    /// The #102 shape: the composing session is torn down by a key-budget
    /// timeout, and the Space that follows must not reach the document.
    /// Exactly one Space is owed to that teardown.
    #[test]
    fn a_teardown_owes_exactly_one_absorption() {
        let fence = CompositionFence::new();
        fence.acquire_owned("claude.exe", 1, 1, None);
        fence.release_owned("claude.exe", 1, 1, true);
        assert!(fence.consume_teardown("claude.exe"));
        assert!(!fence.consume_teardown("claude.exe"));
    }

    /// The invariant that separates the two fences. `any_active` suppresses
    /// every idle key; if a pending teardown showed up there, the session
    /// that replaced the dropped link would lose the characters typed into
    /// it, not just the one Space.
    #[test]
    fn a_pending_teardown_is_invisible_to_any_active() {
        let fence = CompositionFence::new();
        fence.acquire_owned("claude.exe", 1, 1, None);
        fence.release_owned("claude.exe", 1, 1, true);
        assert!(!fence.any_active("claude.exe"));
    }

    #[test]
    fn a_latch_does_not_leak_to_a_different_executable() {
        let fence = CompositionFence::new();
        fence.acquire_owned("claude.exe", 1, 1, None);
        fence.release_owned("claude.exe", 1, 1, true);
        assert!(!fence.any_active("notepad.exe"));
        assert!(!fence.consume_teardown("notepad.exe"));
        assert!(fence.consume_teardown("claude.exe"));
    }

    #[test]
    fn teardown_without_a_claim_arms_nothing() {
        let fence = CompositionFence::new();
        fence.release_owned("claude.exe", 1, 1, true);
        assert!(!fence.any_active("claude.exe"));
        assert!(!fence.consume_teardown("claude.exe"));
    }

    /// Two live contexts, one of which drops: the surviving claim fences the
    /// host on its own, and must not let the dropped one's latch be spent
    /// on a Space that belongs to the live composition.
    #[test]
    fn a_surviving_claim_holds_the_latch_back() {
        let fence = CompositionFence::new();
        fence.acquire_owned("claude.exe", 1, 1, None);
        fence.acquire_owned("claude.exe", 2, 1, None);
        fence.release_owned("claude.exe", 1, 1, true);
        assert!(fence.any_active("claude.exe"));
        assert!(!fence.consume_teardown("claude.exe"));
        fence.release_owned("claude.exe", 2, 1, false);
        assert!(!fence.any_active("claude.exe"));
        assert!(fence.consume_teardown("claude.exe"));
    }

    /// A reconnect that starts composing again disarms the latch, so a
    /// flapping link cannot bank Spaces to swallow later.
    #[test]
    fn a_new_claim_disarms_a_pending_latch() {
        let fence = CompositionFence::new();
        fence.acquire_owned("claude.exe", 1, 1, None);
        fence.release_owned("claude.exe", 1, 1, true);
        fence.acquire_owned("claude.exe", 2, 1, None);
        fence.release_owned("claude.exe", 2, 1, false);
        assert!(!fence.any_active("claude.exe"));
        assert!(!fence.consume_teardown("claude.exe"));
    }

    /// Repeated teardowns owe one Space each, not one in total: the #102
    /// occurrences arrived in bursts.
    #[test]
    fn each_teardown_rearms_the_latch() {
        let fence = CompositionFence::new();
        for _ in 0..3 {
            fence.acquire_owned("claude.exe", 1, 1, None);
            fence.release_owned("claude.exe", 1, 1, true);
            assert!(fence.consume_teardown("claude.exe"));
        }
        assert!(!fence.consume_teardown("claude.exe"));
    }
}
