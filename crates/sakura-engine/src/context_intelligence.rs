//! Engine-owned, bounded context intelligence primitives.
//!
//! This module is deliberately dormant: it defines the per-session semantic
//! context lifecycle and the deterministic local replay baseline for Issue #34,
//! but it is not connected to dispatch, prediction, the neural worker, settings,
//! or the installer yet. Keeping that activation separate avoids colliding with
//! Issue #24's in-progress session and dispatch integration.
//!
//! The context holds only text Sakura definitively committed in one positively
//! classified [`InputScope::Normal`] session. It is allocation-free, never reads
//! host-document text, and exposes redacted `Debug` output so diagnostics cannot
//! accidentally log the retained text.

use core::fmt;

use sakura_neural_proto::Fingerprint;
use sakura_proto::{FixedStr, InputScope};

/// Maximum UTF-8 bytes retained from Sakura-owned commits in one session.
///
/// This is intentionally below the shared wire contract's 512-byte ceiling.
/// The smaller inline store keeps the future 64-session table bounded while
/// leaving room for a separator or a later tokenizer-specific envelope.
pub const MAX_SEMANTIC_CONTEXT_BYTES: usize = 256;

/// Maximum number of recent commit transitions represented by the coarse
/// bounded count. The text tail remains authoritative when older commits have
/// already fallen out of the byte window.
pub const MAX_SEMANTIC_COMMIT_COUNT: u8 = 8;

/// Why engine-owned semantic context was explicitly revoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextClearReason {
    SensitiveScope,
    UnclassifiedScope,
    SessionDeleted,
    ContextReplaced,
    Deactivated,
    OwnershipLost,
    Explicit,
}

/// Observable terminal result of a context lifecycle operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextMutation {
    Retained,
    Appended {
        truncated: bool,
        sentence_boundary: bool,
    },
    Cleared(ContextClearReason),
    RejectedTestOnly,
    RejectedEmpty,
}

/// Immutable, allocation-free snapshot of one session's semantic context.
#[derive(Clone, PartialEq, Eq)]
pub struct SemanticContextSnapshot {
    committed_tail: FixedStr<MAX_SEMANTIC_CONTEXT_BYTES>,
    context_generation: u64,
    sentence_generation: u64,
    last_commit_count: u8,
    fingerprint: Fingerprint,
}

impl SemanticContextSnapshot {
    pub fn committed_tail(&self) -> &str {
        self.committed_tail.as_str()
    }

    pub const fn context_generation(&self) -> u64 {
        self.context_generation
    }

    pub const fn sentence_generation(&self) -> u64 {
        self.sentence_generation
    }

    pub const fn last_commit_count(&self) -> u8 {
        self.last_commit_count
    }

    pub const fn fingerprint(&self) -> &Fingerprint {
        &self.fingerprint
    }
}

impl fmt::Debug for SemanticContextSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SemanticContextSnapshot")
            .field("committed_tail_bytes", &self.committed_tail.len())
            .field("context_generation", &self.context_generation)
            .field("sentence_generation", &self.sentence_generation)
            .field("last_commit_count", &self.last_commit_count)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

/// Volatile semantic context owned by exactly one engine session.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionSemanticContext {
    committed_tail: FixedStr<MAX_SEMANTIC_CONTEXT_BYTES>,
    context_generation: u64,
    sentence_generation: u64,
    last_commit_count: u8,
}

impl SessionSemanticContext {
    pub const fn new() -> Self {
        Self {
            committed_tail: FixedStr::new(),
            // Zero is reserved as an unknown/uninitialized correlation value by
            // the shared contract. A live engine-owned context starts at one.
            context_generation: 1,
            sentence_generation: 1,
            last_commit_count: 0,
        }
    }

    pub fn committed_tail(&self) -> &str {
        self.committed_tail.as_str()
    }

    pub const fn context_generation(&self) -> u64 {
        self.context_generation
    }

    pub const fn sentence_generation(&self) -> u64 {
        self.sentence_generation
    }

    pub const fn last_commit_count(&self) -> u8 {
        self.last_commit_count
    }

    pub fn fingerprint(&self) -> Fingerprint {
        semantic_fingerprint(
            self.committed_tail.as_bytes(),
            self.context_generation,
            self.sentence_generation,
            self.last_commit_count,
        )
    }

    pub fn snapshot(&self) -> SemanticContextSnapshot {
        SemanticContextSnapshot {
            committed_tail: self.committed_tail.clone(),
            context_generation: self.context_generation,
            sentence_generation: self.sentence_generation,
            last_commit_count: self.last_commit_count,
            fingerprint: self.fingerprint(),
        }
    }

    /// Applies the privacy admission rule for a published host scope.
    ///
    /// A positively classified Normal scope retains existing context. Every
    /// other state revokes it, including an unclassified value represented as
    /// `Normal` plus `classified = false`.
    pub fn observe_scope(&mut self, scope: InputScope, classified: bool) -> ContextMutation {
        if classified && scope == InputScope::Normal {
            return ContextMutation::Retained;
        }

        let reason = if !classified || scope == InputScope::Unclassified {
            ContextClearReason::UnclassifiedScope
        } else {
            ContextClearReason::SensitiveScope
        };
        self.clear(reason)
    }

    /// Records only a definitive Sakura commit.
    ///
    /// The caller must invoke this at the commit terminal transition, never
    /// while a document edit is merely planned or in flight. `test_only` is an
    /// explicit pure rejection. Invalid scope clears any retained context so a
    /// missed scope transition cannot leave personal text available.
    pub fn append_definitive_commit(
        &mut self,
        scope: InputScope,
        classified: bool,
        test_only: bool,
        surface: &str,
    ) -> ContextMutation {
        if test_only {
            return ContextMutation::RejectedTestOnly;
        }
        if !classified || scope != InputScope::Normal {
            return self.observe_scope(scope, classified);
        }
        if surface.is_empty() {
            return ContextMutation::RejectedEmpty;
        }

        let truncated = append_utf8_tail(&mut self.committed_tail, surface);
        self.context_generation = next_generation(self.context_generation);
        self.last_commit_count = self
            .last_commit_count
            .saturating_add(1)
            .min(MAX_SEMANTIC_COMMIT_COUNT);
        let sentence_boundary = is_sentence_boundary(surface);
        if sentence_boundary {
            self.sentence_generation = next_generation(self.sentence_generation);
        }
        ContextMutation::Appended {
            truncated,
            sentence_boundary,
        }
    }

    /// Revokes all retained text and advances both correlation generations.
    ///
    /// Generations advance even when the tail is already empty. This makes an
    /// explicit lifecycle boundary observable and invalidates a snapshot taken
    /// before a deactivate/context-replacement pair with no intervening commit.
    pub fn clear(&mut self, reason: ContextClearReason) -> ContextMutation {
        self.committed_tail.clear();
        self.last_commit_count = 0;
        self.context_generation = next_generation(self.context_generation);
        self.sentence_generation = next_generation(self.sentence_generation);
        ContextMutation::Cleared(reason)
    }
}

impl Default for SessionSemanticContext {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for SessionSemanticContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionSemanticContext")
            .field("committed_tail_bytes", &self.committed_tail.len())
            .field("context_generation", &self.context_generation)
            .field("sentence_generation", &self.sentence_generation)
            .field("last_commit_count", &self.last_commit_count)
            .field("fingerprint", &self.fingerprint())
            .finish()
    }
}

fn append_utf8_tail(tail: &mut FixedStr<MAX_SEMANTIC_CONTEXT_BYTES>, surface: &str) -> bool {
    let combined_len = tail.len().saturating_add(surface.len());
    if combined_len <= tail.capacity() {
        let _ = tail.push_str(surface);
        return false;
    }

    let mut replacement = FixedStr::new();
    if surface.len() >= replacement.capacity() {
        let suffix = utf8_suffix(surface, replacement.capacity());
        let _ = replacement.push_str(suffix);
    } else {
        let available = replacement.capacity() - surface.len();
        let retained = utf8_suffix(tail.as_str(), available);
        let _ = replacement.push_str(retained);
        let _ = replacement.push_str(surface);
    }
    *tail = replacement;
    true
}

fn utf8_suffix(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

fn next_generation(current: u64) -> u64 {
    current.saturating_add(1)
}

fn is_sentence_boundary(surface: &str) -> bool {
    let trimmed = surface.trim_end();
    trimmed.ends_with(['。', '！', '？'])
        || trimmed.ends_with("です")
        || trimmed.ends_with("ます")
        || trimmed.ends_with("でした")
        || trimmed.ends_with("ました")
}

fn semantic_fingerprint(
    tail: &[u8],
    context_generation: u64,
    sentence_generation: u64,
    last_commit_count: u8,
) -> Fingerprint {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    const SEEDS: [u64; 4] = [
        0x243f_6a88_85a3_08d3,
        0x1319_8a2e_0370_7344,
        0xa409_3822_299f_31d0,
        0x082e_fa98_ec4e_6c89,
    ];

    let mut input = [0u8; 17];
    input[..8].copy_from_slice(&context_generation.to_le_bytes());
    input[8..16].copy_from_slice(&sentence_generation.to_le_bytes());
    input[16] = last_commit_count;

    let mut fingerprint = [0u8; 32];
    for (lane, seed) in SEEDS.into_iter().enumerate() {
        let mut hash = OFFSET ^ seed;
        for byte in input.iter().chain(tail) {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(PRIME);
        }
        fingerprint[lane * 8..(lane + 1) * 8].copy_from_slice(&hash.to_le_bytes());
    }
    fingerprint
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classified_normal_commit_appends_and_snapshots_without_allocation() {
        let mut context = SessionSemanticContext::new();
        let before = context.fingerprint();

        assert_eq!(
            context.append_definitive_commit(InputScope::Normal, true, false, "日本語を入力",),
            ContextMutation::Appended {
                truncated: false,
                sentence_boundary: false,
            }
        );
        let snapshot = context.snapshot();
        assert_eq!(snapshot.committed_tail(), "日本語を入力");
        assert_eq!(snapshot.context_generation(), 2);
        assert_eq!(snapshot.sentence_generation(), 1);
        assert_eq!(snapshot.last_commit_count(), 1);
        assert_ne!(snapshot.fingerprint(), &before);
    }

    #[test]
    fn utf8_tail_keeps_the_newest_complete_scalars() {
        let mut context = SessionSemanticContext::new();
        let first = "前".repeat(MAX_SEMANTIC_CONTEXT_BYTES / 3);
        assert!(matches!(
            context.append_definitive_commit(InputScope::Normal, true, false, &first),
            ContextMutation::Appended {
                truncated: false,
                ..
            }
        ));

        let newest = "後".repeat(40);
        assert!(matches!(
            context.append_definitive_commit(InputScope::Normal, true, false, &newest),
            ContextMutation::Appended {
                truncated: true,
                ..
            }
        ));
        assert!(context.committed_tail().is_char_boundary(0));
        assert!(context.committed_tail().len() <= MAX_SEMANTIC_CONTEXT_BYTES);
        assert!(context.committed_tail().ends_with(&newest));

        let oversized = "界".repeat(MAX_SEMANTIC_CONTEXT_BYTES);
        context.append_definitive_commit(InputScope::Normal, true, false, &oversized);
        assert!(context.committed_tail().len() <= MAX_SEMANTIC_CONTEXT_BYTES);
        assert!(oversized.ends_with(context.committed_tail()));
    }

    #[test]
    fn test_only_is_pure_and_unknown_or_sensitive_scope_clears() {
        let mut context = SessionSemanticContext::new();
        context.append_definitive_commit(InputScope::Normal, true, false, "private context");
        let before = context.clone();
        assert_eq!(
            context.append_definitive_commit(InputScope::Normal, true, true, "probe"),
            ContextMutation::RejectedTestOnly
        );
        assert_eq!(context, before);

        assert_eq!(
            context.observe_scope(InputScope::Normal, false),
            ContextMutation::Cleared(ContextClearReason::UnclassifiedScope)
        );
        assert!(context.committed_tail().is_empty());

        context.append_definitive_commit(InputScope::Normal, true, false, "ordinary");
        for scope in [
            InputScope::Password,
            InputScope::Url,
            InputScope::Email,
            InputScope::Digits,
        ] {
            assert_eq!(
                context.observe_scope(scope, true),
                ContextMutation::Cleared(ContextClearReason::SensitiveScope)
            );
            assert!(context.committed_tail().is_empty());
        }
    }

    #[test]
    fn every_explicit_lifecycle_clear_revokes_prior_snapshots() {
        let reasons = [
            ContextClearReason::SessionDeleted,
            ContextClearReason::ContextReplaced,
            ContextClearReason::Deactivated,
            ContextClearReason::OwnershipLost,
            ContextClearReason::Explicit,
        ];
        for reason in reasons {
            let mut context = SessionSemanticContext::new();
            context.append_definitive_commit(InputScope::Normal, true, false, "owned");
            let snapshot = context.snapshot();
            assert_eq!(context.clear(reason), ContextMutation::Cleared(reason));
            assert!(context.committed_tail().is_empty());
            assert_eq!(context.last_commit_count(), 0);
            assert_ne!(context.context_generation(), snapshot.context_generation());
            assert_ne!(context.fingerprint(), *snapshot.fingerprint());
        }
    }

    #[test]
    fn sentence_and_commit_generations_are_bounded_and_deterministic() {
        let mut left = SessionSemanticContext::new();
        let mut right = SessionSemanticContext::new();
        for index in 0..16 {
            let surface = if index == 5 {
                "終わりました。"
            } else {
                "続き"
            };
            let left_result =
                left.append_definitive_commit(InputScope::Normal, true, false, surface);
            let right_result =
                right.append_definitive_commit(InputScope::Normal, true, false, surface);
            assert_eq!(left_result, right_result);
        }
        assert_eq!(left.last_commit_count(), MAX_SEMANTIC_COMMIT_COUNT);
        assert_eq!(left.sentence_generation(), 2);
        assert_eq!(left.snapshot(), right.snapshot());
    }

    #[test]
    fn debug_output_redacts_committed_text() {
        let mut context = SessionSemanticContext::new();
        context.append_definitive_commit(InputScope::Normal, true, false, "never-log-this");
        let context_debug = format!("{context:?}");
        let snapshot_debug = format!("{:?}", context.snapshot());
        assert!(!context_debug.contains("never-log-this"));
        assert!(!snapshot_debug.contains("never-log-this"));
        assert!(context_debug.contains("committed_tail_bytes"));
    }

    #[test]
    fn inline_context_has_a_recorded_fixed_size_budget() {
        // 256 bytes of text plus FixedStr length and three scalar fields. Keep
        // the chosen inline representation well below a second preedit buffer.
        let context_bytes = core::mem::size_of::<SessionSemanticContext>();
        let snapshot_bytes = core::mem::size_of::<SemanticContextSnapshot>();
        let current_session_bytes = core::mem::size_of::<crate::session::Session>();
        println!(
            "context-core size: context={context_bytes} snapshot={snapshot_bytes} current-session={current_session_bytes} projected-inline-session={}",
            current_session_bytes.saturating_add(context_bytes)
        );
        assert!(context_bytes <= 296);
        assert!(snapshot_bytes <= 328);
    }
}
