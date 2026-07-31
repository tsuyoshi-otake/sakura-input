//! Per-editing-session state (DESIGN 5, DESIGN 7).
//!
//! One [`Session`] exists per TSF context the DLL has told the engine about
//! (roughly: per focused editable control), and a connection's whole set of
//! them lives in a [`SessionTable`]. Both are pure data plus the small
//! amount of pure logic needed to answer "what state is this session in" —
//! everything that decides what a *keystroke* does to that state lives one
//! layer up, in [`crate::dispatch`], which is what keeps this module
//! testable without a keymap, a romaji table, or a normalizer in scope.
//!
//! # M0 scope
//!
//! Phase 1 has no conversion (DESIGN's later phases add the dictionary and
//! candidate list): a session only ever accumulates a romaji-derived kana
//! reading and commits or discards it whole. That means the
//! [`sakura_core::keymap::State`] a session reports is always either
//! [`State::Idle`] or [`State::Composing`] — never `Converting` or
//! `Predicting` — which is deliberate, not a placeholder still to be wired
//! up: those two states do not exist yet because nothing produces them yet.
//!
//! `Mode::Katakana` and `Mode::HalfKatakana` are tracked here (a session can
//! be *in* either mode) but M0 has no glyph-level hiragana → katakana
//! transform anywhere in the workspace (`sakura_core` ships none), so a
//! session in either mode still composes through the same hiragana-only
//! romaji FSM as `Mode::Hiragana` does. Callers should not read a session
//! reporting `Mode::Katakana` as a promise that its preedit text is
//! katakana yet — see `crate::dispatch`'s module docs for how the
//! dispatcher handles this seam.

use sakura_core::keymap::State;
use sakura_core::romaji;
use sakura_proto::{FixedStr, InputScope, Mode, SessionId, MAX_PREEDIT_BYTES};

/// The most sessions one connection may have live at once.
///
/// A connection is one host application's TSF thread, so this is a per-app
/// bound: it is generous enough that no real application should ever hit it
/// (one session per focused field, and applications do not have hundreds of
/// simultaneously live editable controls), while still keeping the table's
/// footprint — `MAX_SESSIONS * size_of::<Session>()` — a compile-time
/// constant instead of an unbounded allocation a misbehaving or malicious
/// client could grow without limit.
pub const MAX_SESSIONS: usize = 64;

/// The capacity, in UTF-8 bytes, of [`Session::process_name`].
///
/// A host process name (DESIGN's per-app profile key) is a Win32 module file
/// name, not user text — real values are a few dozen bytes at most — so this
/// is sized generously and anything longer is truncated rather than
/// rejected (see [`Session::new`]): a session that cannot be identified
/// precisely is still far more useful than one `CreateSession` refuses to
/// create.
pub const MAX_PROCESS_NAME_BYTES: usize = 128;

/// One editing session's state: what mode it is in, what input scope the
/// focused field reported, and the composition (if any) in progress.
///
/// Cloning a `Session` is a fixed-size, allocation-free copy — every field
/// is a `Copy` value or a stack-resident `FixedStr`/[`romaji::Input`] — which
/// is what lets [`crate::dispatch`] answer a `test_only` key event by
/// running the real logic against a clone and discarding it, instead of
/// needing a separate "what would happen" code path to keep in sync with
/// the real one.
#[derive(Debug, Clone)]
pub struct Session {
    process_name: FixedStr<MAX_PROCESS_NAME_BYTES>,
    /// The IME mode new keystrokes are interpreted under.
    pub(crate) mode: Mode,
    /// The input scope of the field this session belongs to (DESIGN 9).
    pub(crate) scope: InputScope,
    /// Romaji typed but not yet resolved to kana (the FSM's own state).
    pub(crate) romaji: romaji::Input,
    /// Kana (and any unmapped passthrough characters) resolved so far,
    /// *before* the width normalizer runs. `crate::dispatch` normalizes a
    /// copy of this into the wire response; it is never normalized in
    /// place, so that normalizing twice (impossible today, but a change
    /// that made it possible tomorrow) could never double-widen anything.
    pub(crate) preedit: FixedStr<MAX_PREEDIT_BYTES>,
}

impl Session {
    /// Creates a fresh, idle session for `process_name`, starting in
    /// [`Mode::Hiragana`] with [`InputScope::Normal`] and no composition.
    ///
    /// `process_name` is truncated at a UTF-8 character boundary rather
    /// than rejected if it does not fit in [`MAX_PROCESS_NAME_BYTES`]. A
    /// `CreateSession` request comes from the DLL reporting its own host
    /// process's module name, never from untrusted remote input, so an
    /// oversized value is a defensive bound rather than an attack to refuse
    /// outright — refusing to create the session over a field that is only
    /// used for per-app profile lookup and diagnostics would cost far more
    /// (a host application the IME silently does not work in) than it buys.
    pub fn new(process_name: &str) -> Self {
        let mut name = FixedStr::new();
        // `truncate_to_fit` always returns a prefix within capacity on a
        // char boundary, so this push can never overflow.
        let _ = name.push_str(truncate_to_fit(process_name, name.capacity()));
        Session {
            process_name: name,
            mode: Mode::Hiragana,
            scope: InputScope::Normal,
            romaji: romaji::Input::new(),
            preedit: FixedStr::new(),
        }
    }

    /// The host process name this session was created for (possibly
    /// truncated; see [`Session::new`]).
    pub fn process_name(&self) -> &str {
        self.process_name.as_str()
    }

    /// The current IME mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The input scope last reported for this session's field.
    pub fn scope(&self) -> InputScope {
        self.scope
    }

    /// The keymap state this session is in.
    ///
    /// M0 has no conversion, so the only two states a session can ever
    /// report are [`State::Idle`] (nothing pending) and [`State::Composing`]
    /// (romaji is pending, or kana has been resolved and not yet committed
    /// or cancelled) — see the module docs.
    pub fn state(&self) -> State {
        if self.is_composing() {
            State::Composing
        } else {
            State::Idle
        }
    }

    /// `true` if there is a composition in progress: pending romaji, or
    /// kana already resolved from it and not yet committed or cancelled.
    pub fn is_composing(&self) -> bool {
        !self.romaji.is_empty() || !self.preedit.is_empty()
    }

    /// Discards any composition in progress, back to a clean idle session.
    ///
    /// Deliberately leaves `mode` and `scope` untouched: those describe how
    /// the *user* wants to type and what kind of field they are typing
    /// into, neither of which a cancelled composition has anything to say
    /// about. A `Cancel` action (Escape) that quietly reset the mode back
    /// to Hiragana out from under someone typing in Katakana mode would be
    /// a surprise bug wearing a "just resetting" disguise.
    pub fn reset(&mut self) {
        self.romaji.clear();
        self.preedit.clear();
    }
}

/// The longest prefix of `s` that both fits in `cap` bytes and ends on a
/// UTF-8 character boundary.
///
/// Unlike [`FixedStr::push_str`], which fails atomically rather than ever
/// writing a partial value, this is the one place in the crate that is
/// meant to accept a partial value on purpose (see [`Session::new`]).
fn truncate_to_fit(s: &str, cap: usize) -> &str {
    if s.len() <= cap {
        return s;
    }
    let mut end = cap;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A fixed-capacity, allocation-free map from [`SessionId`] to [`Session`].
///
/// Backed by `[Option<(SessionId, Session)>; MAX_SESSIONS]` rather than a
/// `HashMap` — this crate's whole point is a bounded, allocation-free
/// footprint per connection (DESIGN 5.7, DESIGN 10's 15 MB budget), and a
/// `HashMap` is exactly the unbounded-growth, heap-backed structure that
/// contradicts it. It is also not `sakura_proto::FixedVec<(SessionId,
/// Session), N>`: `FixedVec` requires `T: Copy + Default` so that an empty
/// slot needs no drop handling, and `Session` holds a `FixedStr` (`Clone`,
/// not `Copy`, so that cloning stays an explicit, visible cost — see
/// [`Session`]'s docs). An array of `Option` needs no such bound and costs
/// nothing extra: a `None` slot is exactly as cheap as an unused `FixedVec`
/// slot would have been.
#[derive(Debug)]
pub struct SessionTable {
    slots: [Option<(SessionId, Session)>; MAX_SESSIONS],
    len: usize,
    /// The id the *next* `create` will hand out. Monotonic for the whole
    /// life of the table: never reset, never reused, even for an id whose
    /// session has since been deleted (see [`SessionTable::create`]).
    next_id: SessionId,
}

impl SessionTable {
    /// An empty table. Ids handed out by [`SessionTable::create`] start at
    /// 1 — 0 is never a live session id, which lets a caller use 0 as an
    /// "no session" sentinel if it ever needs one without that colliding
    /// with a real id.
    pub fn new() -> Self {
        SessionTable {
            slots: core::array::from_fn(|_| None),
            len: 0,
            next_id: 1,
        }
    }

    /// The number of live sessions.
    pub fn len(&self) -> usize {
        self.len
    }

    /// `true` if no session is live.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Creates a new session for `process_name` and returns its id.
    ///
    /// Fails with [`sakura_proto::ErrorCode::Busy`] once [`MAX_SESSIONS`]
    /// sessions are already live — the table does not evict anything to
    /// make room, because a session belongs to one focused field in one
    /// host application and evicting a live one out from under it would be
    /// exactly the "lose user text" DESIGN 1 rules out.
    pub fn create(&mut self, process_name: &str) -> Result<SessionId, sakura_proto::ErrorCode> {
        if self.len >= MAX_SESSIONS {
            return Err(sakura_proto::ErrorCode::Busy);
        }
        let slot = self
            .slots
            .iter_mut()
            .find(|slot| slot.is_none())
            .expect("len < MAX_SESSIONS guarantees a free slot");
        let id = self.next_id;
        // Saturating, not wrapping: wrapping back to id 1 (or worse, a
        // still-live id) would let a stale request from long ago resolve
        // against an unrelated session that happens to reuse its id. A
        // `u64` counter incremented at most once per `CreateSession` has no
        // real-world path to exhausting this; saturating just means that
        // impossible case fails safely (refuses new sessions) instead of
        // unsafely (aliases an old one).
        self.next_id = self.next_id.saturating_add(1);
        *slot = Some((id, Session::new(process_name)));
        self.len += 1;
        Ok(id)
    }

    /// The session named by `id`, if it is live.
    pub fn get(&self, id: SessionId) -> Option<&Session> {
        self.slots
            .iter()
            .flatten()
            .find(|(sid, _)| *sid == id)
            .map(|(_, session)| session)
    }

    /// A mutable handle to the session named by `id`, if it is live.
    pub fn get_mut(&mut self, id: SessionId) -> Option<&mut Session> {
        self.slots
            .iter_mut()
            .flatten()
            .find(|(sid, _)| *sid == id)
            .map(|(_, session)| session)
    }

    /// Removes the session named by `id`. Returns `true` if it was live.
    ///
    /// The id is never handed back out by a later [`SessionTable::create`]
    /// (see its docs): a stale reply or a delayed request that still names
    /// this id must keep failing with `UnknownSession` forever, not start
    /// resolving against whatever session happens to reuse the number.
    pub fn delete(&mut self, id: SessionId) -> bool {
        for slot in &mut self.slots {
            if matches!(slot, Some((sid, _)) if *sid == id) {
                *slot = None;
                self.len -= 1;
                return true;
            }
        }
        false
    }

    /// Removes every session, without resetting the id counter.
    ///
    /// This is what a new connection starts from (`crate::dispatch`'s
    /// `Dispatcher::reset`, called between connections on the same pipe
    /// instance): the sessions themselves belong to the connection that is
    /// gone, but the id counter keeps counting up regardless, for the same
    /// stale-id-must-not-resolve reason `delete` never reuses one.
    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            *slot = None;
        }
        self.len = 0;
    }
}

impl Default for SessionTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_proto::ErrorCode;

    #[test]
    fn a_new_session_starts_idle_in_hiragana_mode_with_normal_scope() {
        let session = Session::new("notepad.exe");
        assert_eq!(session.process_name(), "notepad.exe");
        assert_eq!(session.mode(), Mode::Hiragana);
        assert_eq!(session.scope(), InputScope::Normal);
        assert_eq!(session.state(), State::Idle);
        assert!(!session.is_composing());
    }

    #[test]
    fn an_oversized_process_name_is_truncated_on_a_char_boundary_not_rejected() {
        // Every character is 3 bytes, so the exact byte cap lands
        // mid-character; truncation must back off to the char before it
        // rather than split it or panic.
        let long = "あ".repeat(MAX_PROCESS_NAME_BYTES); // MAX_PROCESS_NAME_BYTES / 3 chars short of MAX_PROCESS_NAME_BYTES * 3 bytes
        let session = Session::new(&long);
        assert!(session.process_name().len() <= MAX_PROCESS_NAME_BYTES);
        assert!(long.starts_with(session.process_name()));
        assert!(session
            .process_name()
            .is_char_boundary(session.process_name().len()));
    }

    #[test]
    fn reset_clears_the_composition_but_not_mode_or_scope() {
        let mut session = Session::new("cmd.exe");
        session.mode = Mode::Katakana;
        session.scope = InputScope::Url;
        session.preedit.push_str("か").expect("fits");
        assert!(session.is_composing());

        session.reset();

        assert!(!session.is_composing());
        assert_eq!(session.state(), State::Idle);
        assert_eq!(session.mode(), Mode::Katakana);
        assert_eq!(session.scope(), InputScope::Url);
    }

    #[test]
    fn pending_romaji_alone_counts_as_composing() {
        // "k" alone never resolves on its own (it waits for a vowel), so
        // feeding it leaves `preedit` empty and only `romaji` pending --
        // exactly the case `is_composing`/`state` must not miss.
        let table = romaji::Table::builtin().expect("the shipped romaji table compiles");
        let mut session = Session::new("cmd.exe");
        table
            .feed(&mut session.romaji, 'k', &mut session.preedit)
            .expect("fits");
        assert!(session.preedit.is_empty());
        assert!(!session.romaji.is_empty());
        assert!(session.is_composing());
        assert_eq!(session.state(), State::Composing);
    }

    #[test]
    fn a_fresh_session_table_hands_out_ids_starting_at_one() {
        let mut table = SessionTable::new();
        assert!(table.is_empty());
        let id = table.create("a.exe").expect("room for one session");
        assert_eq!(id, 1);
        assert_eq!(table.len(), 1);
        assert!(!table.is_empty());
    }

    #[test]
    fn created_sessions_are_reachable_by_get_and_get_mut() {
        let mut table = SessionTable::new();
        let id = table.create("a.exe").expect("room");
        assert_eq!(table.get(id).map(Session::process_name), Some("a.exe"));
        table.get_mut(id).expect("live").mode = Mode::FullAlnum;
        assert_eq!(table.get(id).map(Session::mode), Some(Mode::FullAlnum));
    }

    #[test]
    fn an_unknown_id_resolves_to_nothing() {
        let mut table = SessionTable::new();
        assert!(table.get(999).is_none());
        assert!(table.get_mut(999).is_none());
        assert!(!table.delete(999));
    }

    #[test]
    fn deleting_a_session_frees_its_slot_but_never_its_id() {
        let mut table = SessionTable::new();
        let first = table.create("a.exe").expect("room");
        assert!(table.delete(first));
        assert_eq!(table.len(), 0);
        assert!(table.get(first).is_none());

        let second = table.create("b.exe").expect("room");
        assert_ne!(first, second, "a deleted id must never be handed out again");
        assert!(second > first);
    }

    #[test]
    fn the_table_reports_busy_once_full_and_recovers_after_a_delete() {
        let mut table = SessionTable::new();
        let mut ids = Vec::new();
        for _ in 0..MAX_SESSIONS {
            ids.push(table.create("app.exe").expect("under the cap"));
        }
        assert_eq!(table.len(), MAX_SESSIONS);
        assert_eq!(table.create("one.exe"), Err(ErrorCode::Busy));

        // Freeing one slot makes room for exactly one more.
        assert!(table.delete(ids[0]));
        assert!(table.create("another.exe").is_ok());
        assert_eq!(table.create("yet-another.exe"), Err(ErrorCode::Busy));
    }

    #[test]
    fn clear_empties_the_table_without_resetting_the_id_counter() {
        let mut table = SessionTable::new();
        table.create("a.exe").expect("room");
        table.create("b.exe").expect("room");
        table.clear();
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);

        // The next id keeps counting up from where it left off, not from 1
        // again — a stale request naming an id from before the clear must
        // not be able to resolve against whatever gets created after it.
        let id = table.create("c.exe").expect("room");
        assert_eq!(id, 3);
    }

    #[test]
    fn default_matches_new() {
        assert_eq!(SessionTable::default().len(), SessionTable::new().len());
    }
}
