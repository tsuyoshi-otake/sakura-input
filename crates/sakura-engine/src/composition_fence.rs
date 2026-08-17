//! Process-wide composition activity for idle Space absorption.
//!
//! Engine pipe workers are share-nothing: each connection owns its own
//! [`crate::dispatch::Dispatcher`]. Electron / Cursor can still deliver one
//! physical Space to every live TSF context of the same host process. When
//! one of those contexts is composing or converting, an idle peer must not
//! insert a document space for that same Space key.
//!
//! Counts are keyed by host `process_name` (what CreateSession reports).
//! Distinct executables do not fence each other. Same-name processes share
//! a count — a deliberate bound when two Notepad windows both report
//! `notepad.exe`.

use std::collections::HashMap;
use std::sync::Mutex;

/// Shared across every pipe worker in one engine process.
#[derive(Debug, Default)]
pub struct CompositionFence {
    /// Number of live composing/converting claims per host process name.
    counts: Mutex<HashMap<Box<str>, u32>>,
}

impl CompositionFence {
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` when any connection for `process_name` currently claims an
    /// active composition or conversion.
    pub fn any_active(&self, process_name: &str) -> bool {
        self.counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(process_name)
            .copied()
            .unwrap_or(0)
            > 0
    }

    pub fn acquire(&self, process_name: &str) {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *counts.entry(Box::from(process_name)).or_insert(0) += 1;
    }

    pub fn release(&self, process_name: &str) {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(count) = counts.get_mut(process_name) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            counts.remove(process_name);
        }
    }
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
    fn two_claims_keep_the_host_active_until_both_release() {
        let fence = CompositionFence::new();
        fence.acquire("app.exe");
        fence.acquire("app.exe");
        fence.release("app.exe");
        assert!(fence.any_active("app.exe"));
        fence.release("app.exe");
        assert!(!fence.any_active("app.exe"));
    }
}
