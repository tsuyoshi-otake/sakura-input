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
        let key = normalize_process_name(process_name);
        self.counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key.as_ref())
            .copied()
            .unwrap_or(0)
            > 0
    }

    pub fn acquire(&self, process_name: &str) {
        let key = normalize_process_name(process_name);
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *counts.entry(key).or_insert(0) += 1;
    }

    pub fn release(&self, process_name: &str) {
        let key = normalize_process_name(process_name);
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(count) = counts.get_mut(key.as_ref()) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            counts.remove(key.as_ref());
        }
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
