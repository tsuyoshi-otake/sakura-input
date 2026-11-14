//! The bounded volatile tail of text a session committed (#471 phase 2).
//!
//! The type is public so the `sakura-conversion-eval` measurements for #474
//! feed their context through exactly this implementation — the same
//! 256-byte bound, oldest-first discard and UTF-8 boundary rules the engine
//! enforces — instead of carrying a second copy that could drift. Widening
//! this boundary is deliberate and minimal: `Session` remains the only
//! writer inside the engine, and the type itself is pure data with no
//! session, history or persistence access.

use std::fmt;

use sakura_proto::FixedStr;

/// The capacity, in UTF-8 bytes, of [`Session::committed_context`].
///
/// The committed-context tail only needs to reach back far enough to hold
/// the words that would decide a homophone: median commits are two
/// characters and the observed clues sit within about the previous twelve,
/// so ~85 Japanese characters is well past the useful window while the
/// whole field stays stack-priced for Probe clones.
///
/// [`Session::committed_context`]: crate::session::Session::committed_context
pub const MAX_COMMITTED_CONTEXT_BYTES: usize = 256;

/// Volatile tail of text a session committed while a classified
/// [`InputScope::Normal`] field was focused (#471 phase 2).
///
/// [`Session::record_current_commit_with_cache`] is the only writer and
/// every [`Session::clear_personal_context`] path drops it, so the text
/// inherits exactly the lifetime rules the rest of the personal-context
/// state already enforces. It is never persisted, recorded to the history
/// sink, or sent off-session; its `Debug` impl prints the byte length only.
///
/// [`InputScope::Normal`]: sakura_proto::InputScope::Normal
/// [`Session::record_current_commit_with_cache`]: crate::session::Session::record_current_commit_with_cache
/// [`Session::clear_personal_context`]: crate::session::Session::clear_personal_context
#[derive(Clone, PartialEq, Eq, Default)]
pub struct CommittedContext {
    tail: FixedStr<MAX_COMMITTED_CONTEXT_BYTES>,
}

impl CommittedContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn as_str(&self) -> &str {
        self.tail.as_str()
    }

    pub fn clear(&mut self) {
        self.tail.clear();
    }

    /// Appends `surface`, dropping the oldest bytes on a char boundary when
    /// the commit overflows the bound.
    pub fn append(&mut self, surface: &str) {
        if self.tail.len() + surface.len() <= self.tail.capacity() {
            // The bounded append cannot fail: the sum fits.
            let _ = self.tail.push_str(surface);
            return;
        }
        let mut replacement = FixedStr::new();
        if surface.len() >= replacement.capacity() {
            let _ = replacement.push_str(utf8_suffix(surface, replacement.capacity()));
        } else {
            let retained = replacement.capacity() - surface.len();
            let _ = replacement.push_str(utf8_suffix(self.tail.as_str(), retained));
            let _ = replacement.push_str(surface);
        }
        self.tail = replacement;
    }

    /// Removes `surface` when it is the most recently appended text — the
    /// commit-undo terminal, where the host deleted exactly that text. Any
    /// other result means the middle was edited, which this bounded trail
    /// cannot see; the bytes that stay are still bytes the session
    /// committed, so they stay.
    pub(crate) fn remove_suffix(&mut self, surface: &str) -> bool {
        if surface.is_empty() || !self.tail.as_str().ends_with(surface) {
            return false;
        }
        let keep = self.tail.as_str().chars().count() - surface.chars().count();
        self.tail.truncate_chars(keep);
        true
    }
}

impl fmt::Debug for CommittedContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CommittedContext")
            .field("tail_bytes", &self.tail.len())
            .finish()
    }
}

/// The longest UTF-8 suffix of `text` that fits in `max_bytes`, aligned to a
/// char boundary.
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
