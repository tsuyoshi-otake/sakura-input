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
//! [`CompositionFence::release_after_teardown`] arms a one-shot latch for
//! that host so the Space right after a broken link can be absorbed.
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
//! [`crate::space_key_dispatch_oracle`] models it exactly rather than
//! approximately. A composition that ends the way it was meant to still
//! uses [`CompositionFence::release`] and arms nothing.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, MutexGuard};

/// Shared across every pipe worker in one engine process.
#[derive(Debug, Default)]
pub struct CompositionFence {
    state: Mutex<FenceState>,
}

#[derive(Debug, Default)]
struct FenceState {
    /// Number of live composing/converting claims per host process name.
    counts: HashMap<Box<str>, u32>,
    /// Hosts that lost a live reading to a teardown and have not yet spent
    /// the one Space that loss entitles them to absorb.
    torn_down: HashSet<Box<str>>,
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
        self.lock()
            .counts
            .get(key.as_ref())
            .copied()
            .unwrap_or_default()
            > 0
    }

    pub fn acquire(&self, process_name: &str) {
        let key = normalize_process_name(process_name);
        let mut state = self.lock();
        // Composing again is proof the link recovered, so the teardown has
        // nothing left to protect and its latch must not survive to eat a
        // Space the user types much later.
        state.torn_down.remove(key.as_ref());
        *state.counts.entry(key).or_insert(0) += 1;
    }

    /// The composition ended the way it was supposed to — committed,
    /// cancelled, or replaced. The host stops fencing immediately.
    pub fn release(&self, process_name: &str) {
        let key = normalize_process_name(process_name);
        self.lock().release_count(key.as_ref());
    }

    /// The claim was torn down while its reading was still live: the
    /// session was deleted mid-composition, or its connection reset. Arms
    /// the one-shot latch so the Space that follows a dropped link is not
    /// committed as a document space by the session that replaced it.
    pub fn release_after_teardown(&self, process_name: &str) {
        let key = normalize_process_name(process_name);
        let mut state = self.lock();
        // Only a host that was actually composing has anything to protect.
        // A teardown with no claim arms nothing, so an ordinary idle Space
        // still inserts its space.
        if state.release_count(key.as_ref()) {
            state.torn_down.insert(key);
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
        // A live claim already fences this host through `any_active`. Keep
        // the latch for the teardown it was armed for.
        if state.counts.get(key.as_ref()).copied().unwrap_or_default() > 0 {
            return false;
        }
        state.torn_down.remove(key.as_ref())
    }

    fn lock(&self) -> MutexGuard<'_, FenceState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl FenceState {
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
    fn peer_becomes_active_only_after_acquire() {
        let fence = CompositionFence::new();
        assert!(!fence.any_active("cursor.exe"));
        fence.acquire("cursor.exe");
        assert!(fence.any_active("cursor.exe"));
        assert!(!fence.any_active("notepad.exe"));
        fence.release("cursor.exe");
        assert!(!fence.any_active("cursor.exe"));
    }

    #[test]
    fn process_name_matching_is_ascii_case_insensitive() {
        let fence = CompositionFence::new();
        fence.acquire("Cursor.exe");
        assert!(fence.any_active("cursor.exe"));
        fence.release("CURSOR.EXE");
        assert!(!fence.any_active("Cursor.exe"));
        fence.acquire("Cursor.exe");
        fence.release_after_teardown("CURSOR.EXE");
        assert!(fence.consume_teardown("cursor.exe"));
    }

    #[test]
    fn two_claims_keep_the_host_active_until_both_release() {
        let fence = CompositionFence::new();
        fence.acquire("app.exe");
        fence.acquire("app.exe");
        fence.release("app.exe");
        assert!(fence.any_active("app.exe"));
        fence.release("app.exe");
        assert!(!fence.any_active("app.exe"));
    }

    /// The #102 shape: the composing session is torn down by a key-budget
    /// timeout, and the Space that follows must not reach the document.
    /// Exactly one Space is owed to that teardown.
    #[test]
    fn a_teardown_owes_exactly_one_absorption() {
        let fence = CompositionFence::new();
        fence.acquire("claude.exe");
        fence.release_after_teardown("claude.exe");
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
        fence.acquire("claude.exe");
        fence.release_after_teardown("claude.exe");
        assert!(!fence.any_active("claude.exe"));
    }

    #[test]
    fn a_latch_does_not_leak_to_a_different_executable() {
        let fence = CompositionFence::new();
        fence.acquire("claude.exe");
        fence.release_after_teardown("claude.exe");
        assert!(!fence.any_active("notepad.exe"));
        assert!(!fence.consume_teardown("notepad.exe"));
        assert!(fence.consume_teardown("claude.exe"));
    }

    #[test]
    fn teardown_without_a_claim_arms_nothing() {
        let fence = CompositionFence::new();
        fence.release_after_teardown("claude.exe");
        assert!(!fence.any_active("claude.exe"));
        assert!(!fence.consume_teardown("claude.exe"));
    }

    /// Two live contexts, one of which drops: the surviving claim fences the
    /// host on its own, and must not let the dropped one's latch be spent
    /// on a Space that belongs to the live composition.
    #[test]
    fn a_surviving_claim_holds_the_latch_back() {
        let fence = CompositionFence::new();
        fence.acquire("claude.exe");
        fence.acquire("claude.exe");
        fence.release_after_teardown("claude.exe");
        assert!(fence.any_active("claude.exe"));
        assert!(!fence.consume_teardown("claude.exe"));
        fence.release("claude.exe");
        assert!(!fence.any_active("claude.exe"));
        assert!(fence.consume_teardown("claude.exe"));
    }

    /// A reconnect that starts composing again disarms the latch, so a
    /// flapping link cannot bank Spaces to swallow later.
    #[test]
    fn a_new_claim_disarms_a_pending_latch() {
        let fence = CompositionFence::new();
        fence.acquire("claude.exe");
        fence.release_after_teardown("claude.exe");
        fence.acquire("claude.exe");
        fence.release("claude.exe");
        assert!(!fence.any_active("claude.exe"));
        assert!(!fence.consume_teardown("claude.exe"));
    }

    /// Repeated teardowns owe one Space each, not one in total: the #102
    /// occurrences arrived in bursts.
    #[test]
    fn each_teardown_rearms_the_latch() {
        let fence = CompositionFence::new();
        for _ in 0..3 {
            fence.acquire("claude.exe");
            fence.release_after_teardown("claude.exe");
            assert!(fence.consume_teardown("claude.exe"));
        }
        assert!(!fence.consume_teardown("claude.exe"));
    }
}
