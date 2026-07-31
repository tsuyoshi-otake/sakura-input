//! Request in, response out.
//!
//! [`Dispatcher`] is the whole engine's pure-logic core: it owns the shipped
//! romaji table, key map, and width normalizer, a bounded table of live
//! sessions (see [`crate::session`]), and one scratch buffer, and it turns
//! one [`Request`] plus a session's state into one [`Reply`] plus whatever
//! got written into the caller's [`OutputBuf`]. Nothing in this module opens
//! a handle, spawns a thread, or reads a clock — see the crate's module
//! docs for why that split exists — which is what lets every behaviour
//! below be driven from a `#[test]` instead of a running pipe.
//!
//! # M0 scope
//!
//! Phase 1 (DESIGN's M0) has no conversion: romaji in, hiragana preedit out,
//! Enter commits it whole. [`Action`] has forty variants because the key
//! map format already describes every phase's bindings, but this dispatcher
//! only *implements* the handful M0 needs (`Commit`, `Cancel`, `DeleteBack`,
//! the mode switches, and the IME on/off toggles); everything else a key
//! map might resolve to — conversion, candidate movement, prediction,
//! segment editing, transforms, reconversion — is accepted and silently
//! swallowed (see [`apply_action`]'s fallback arm) rather than crashing or
//! falling through into the host document, leaving a clean seam for the
//! phase that implements it to fill in one match arm at a time.
//!
//! `Mode::Katakana` and `Mode::HalfKatakana` compose through the same
//! hiragana-only romaji FSM as `Mode::Hiragana` (see [`crate::session`]'s
//! module docs): M0 ships no glyph transform between kana forms, so all
//! three kana modes are, for now, one mode wearing three names. Only
//! `Mode::HalfAlnum` and `Mode::FullAlnum` behave distinctly today — they
//! never build a composition at all, committing each keystroke immediately
//! through the width normalizer (see [`apply_alnum_char`]).

use sakura_core::keymap::{Action, KeyMap, KeyMapError, Preset};
use sakura_core::romaji::{Table, TableError};
use sakura_core::width::Normalizer;
use sakura_proto::{
    ErrorCode, FixedStr, InputScope, KeyInput, Mode, OutputBuf, Overflow, Request, Response,
    SessionId, UnderlineKind, MAX_PREEDIT_BYTES, PROTOCOL_VERSION,
};

use crate::session::{Session, SessionTable};

/// Reported to a client that asks `Hello`. Not the protocol version (that is
/// [`PROTOCOL_VERSION`], checked separately by `sakura_proto::decode_request`
/// before a request ever reaches here) — this is the engine build itself,
/// for diagnostics. There is no released engine yet to track compatibility
/// against, so it simply mirrors the workspace version.
const ENGINE_VERSION: [u16; 3] = [0, 1, 0];

/// Why [`Dispatcher::new`] could not build itself from the shipped defaults.
///
/// Both sources are compiled from data files this same workspace ships
/// (`Table::builtin`, `KeyMap::preset`), so in a correctly built binary this
/// is unreachable; it exists so a corrupted or mis-packaged build fails with
/// a diagnosable message (`server.rs` logs its `Display` output) instead of
/// a panic with no context.
#[derive(Debug)]
pub enum NewError {
    Romaji(TableError),
    KeyMap(KeyMapError),
}

impl core::fmt::Display for NewError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            NewError::Romaji(error) => write!(f, "romaji table: {error}"),
            NewError::KeyMap(error) => write!(f, "key map: {error}"),
        }
    }
}

impl std::error::Error for NewError {}

/// What answering a [`Request`] produced.
///
/// [`Dispatcher::dispatch`] always writes into the caller's [`OutputBuf`] (it
/// starts every call by clearing it), but only `Output` means that buffer is
/// the answer; `Message` and `Shutdown` carry their own [`Response`] and the
/// `OutputBuf` for that call is left empty. Keeping these as one enum rather
/// than always returning a `Response` is what lets `SendKey`'s hot path
/// answer through the allocation-free `OutputBuf`/`encode_frame` route while
/// every other request still gets the ordinary allocating `Response` path
/// (`server.rs` branches on exactly this).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    /// The answer is in the `OutputBuf` passed to `dispatch`.
    Output,
    /// The answer is this `Response`; the `OutputBuf` is empty.
    Message(Response),
    /// The client asked the engine to stop. Answer with this `Response`,
    /// then stop serving — see `crate::server`'s module docs for why
    /// acting on it is the caller's job, not this one's.
    Shutdown(Response),
}

/// The engine's whole pure-logic core for one connection.
///
/// A `Dispatcher` owns everything a session's keystrokes are resolved
/// against — the romaji table, the key map, the width normalizer — plus the
/// bounded table of sessions those keystrokes belong to and one scratch
/// buffer used to build normalized text before it is copied into an
/// `OutputBuf`. Per `crate::server`'s share-nothing design, one
/// `Dispatcher` belongs to one pipe-instance thread for that thread's whole
/// life, reused across sequential connections via [`Dispatcher::reset`].
#[derive(Debug)]
pub struct Dispatcher {
    table: Table,
    keymap: KeyMap,
    normalizer: Normalizer,
    sessions: SessionTable,
    /// Scratch space for building normalized text before it is copied into
    /// an `OutputBuf` segment or commit field. Living here rather than as a
    /// stack-local in the functions that use it is what keeps the `SendKey`
    /// path allocation-free: a stack-local `FixedStr` would be reinitialized
    /// (and, at debug-build stack-probe granularity, freshly touched) on
    /// every call, where this one is zeroed once, for the dispatcher's
    /// whole life.
    scratch: FixedStr<MAX_PREEDIT_BYTES>,
}

impl Dispatcher {
    /// Builds a dispatcher from the engine's shipped defaults: the built-in
    /// romaji table, the MS-IME key map preset, and a default (never-widen)
    /// width normalizer.
    pub fn new() -> Result<Self, NewError> {
        let table = Table::builtin().map_err(NewError::Romaji)?;
        let keymap = KeyMap::preset(Preset::MsIme).map_err(NewError::KeyMap)?;
        Ok(Self::with_parts(table, keymap, Normalizer::default()))
    }

    /// Builds a dispatcher from already-built parts. Used by tests that need
    /// a romaji table, key map, or normalizer the shipped defaults cannot
    /// reach (for example the MS-IME preset binds no key to a direct
    /// mode-switch action, so exercising `Mode::FullAlnum` needs a key map
    /// that does).
    pub fn with_parts(table: Table, keymap: KeyMap, normalizer: Normalizer) -> Self {
        Dispatcher {
            table,
            keymap,
            normalizer,
            sessions: SessionTable::new(),
            scratch: FixedStr::new(),
        }
    }

    /// Drops every session, ready for a new connection to start clean.
    ///
    /// Leaves the romaji table, key map, and normalizer untouched — those
    /// are the engine's shipped configuration, not connection state. See
    /// `crate::server`'s `worker`, which calls this between connections on
    /// the same pipe instance.
    pub fn reset(&mut self) {
        self.sessions.clear();
    }

    /// Answers one request, writing `Output` replies into `out`.
    ///
    /// `out` is cleared unconditionally before anything else, so a caller
    /// that reuses the same `OutputBuf` across calls (as `crate::server`
    /// does) never sees a previous call's leftovers on a `Message` or
    /// `Shutdown` reply.
    pub fn dispatch(&mut self, request: &Request, out: &mut OutputBuf) -> Reply {
        out.clear();
        match request {
            Request::Hello { client_version } => self.hello(*client_version),
            Request::CreateSession { process_name } => self.create_session(process_name),
            Request::SendKey { session, key } => self.send_key(*session, key, out),
            Request::Commit { session } => self.commit(*session, out),
            Request::Revert { session } => self.revert(*session),
            Request::SetInputScope { session, scope } => self.set_input_scope(*session, *scope),
            Request::DeleteSession { session } => self.delete_session(*session),
            Request::Ping => Reply::Message(Response::Pong),
            Request::Shutdown => Reply::Shutdown(Response::Ok),
        }
    }

    fn hello(&self, client_version: u16) -> Reply {
        if client_version == PROTOCOL_VERSION {
            Reply::Message(Response::Hello {
                server_version: PROTOCOL_VERSION,
                engine_version: ENGINE_VERSION,
            })
        } else {
            Reply::Message(Response::Error(ErrorCode::UnsupportedVersion))
        }
    }

    fn create_session(&mut self, process_name: &str) -> Reply {
        match self.sessions.create(process_name) {
            Ok(session) => Reply::Message(Response::SessionCreated { session }),
            Err(code) => Reply::Message(Response::Error(code)),
        }
    }

    fn delete_session(&mut self, id: SessionId) -> Reply {
        if self.sessions.delete(id) {
            Reply::Message(Response::Ok)
        } else {
            Reply::Message(Response::Error(ErrorCode::UnknownSession))
        }
    }

    fn set_input_scope(&mut self, id: SessionId, scope: InputScope) -> Reply {
        let Some(session) = self.sessions.get_mut(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        session.scope = scope;
        if scope == InputScope::Password {
            // DESIGN 9: a sensitive scope bypasses composition entirely, so
            // whatever was in progress is discarded, not flushed -- flushing
            // it would be putting text the user typed before the field was
            // known to be sensitive into the document, which is the exact
            // leak DESIGN 9 exists to prevent.
            session.reset();
            session.mode = Mode::Direct;
        }
        Reply::Message(Response::Ok)
    }

    fn send_key(&mut self, id: SessionId, key: &KeyInput, out: &mut OutputBuf) -> Reply {
        let Some(session) = self.sessions.get_mut(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };

        if key.test_only {
            // Run the real logic against a throwaway clone so the answer
            // (`out.consumed` above all) is exactly what a real dispatch
            // would produce, without a second code path to keep in sync
            // with the first one and without ever writing back to the live
            // session. `Session` is plain stack data (see its docs), so
            // this clone is not the allocation the zero-alloc test cares
            // about -- that test only asserts on the non-`test_only` path.
            let mut probe = session.clone();
            let _ = apply_key(
                &mut probe,
                &self.table,
                &self.keymap,
                &self.normalizer,
                key,
                &mut self.scratch,
                out,
            );
            return Reply::Output;
        }

        match apply_key(
            session,
            &self.table,
            &self.keymap,
            &self.normalizer,
            key,
            &mut self.scratch,
            out,
        ) {
            Ok(()) => Reply::Output,
            Err(Overflow) => {
                // The composition (or the host's `OutputBuf`) would not
                // fit. Reset rather than leave a half-updated composition
                // the session can never make progress from again -- see
                // this module's and the crate's docs on never wedging a
                // session.
                session.reset();
                out.clear();
                Reply::Message(Response::Error(ErrorCode::TooLarge))
            }
        }
    }

    fn commit(&mut self, id: SessionId, out: &mut OutputBuf) -> Reply {
        let Some(session) = self.sessions.get_mut(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        match commit_pending(
            session,
            &self.table,
            &self.normalizer,
            &mut self.scratch,
            out,
        ) {
            Ok(()) => Reply::Output,
            Err(Overflow) => {
                session.reset();
                out.clear();
                Reply::Message(Response::Error(ErrorCode::TooLarge))
            }
        }
    }

    fn revert(&mut self, id: SessionId) -> Reply {
        let Some(session) = self.sessions.get_mut(id) else {
            return Reply::Message(Response::Error(ErrorCode::UnknownSession));
        };
        session.reset();
        Reply::Message(Response::Ok)
    }
}

/// Resolves one keystroke against `session`, mutating its composition and
/// writing the visible result into `out`. Free rather than a `Dispatcher`
/// method so its callers (`Dispatcher::send_key`, and transitively every
/// mode switch) can hold a `&mut Session` borrowed from `self.sessions`
/// alongside `&self.table`/`&self.keymap`/`&self.normalizer`/`&mut
/// self.scratch` at the same time -- those are disjoint fields of the same
/// `Dispatcher`, which the borrow checker can only see through when they
/// are named directly, not funnelled through a `&mut self` method.
fn apply_key(
    session: &mut Session,
    table: &Table,
    keymap: &KeyMap,
    normalizer: &Normalizer,
    key: &KeyInput,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    if session.mode == Mode::Direct {
        // DESIGN 9: a full bypass. No keymap lookup, no romaji processing,
        // nothing built -- the safest reading of "sensitive scopes bypass
        // composition" is that nothing about what is typed is ever seen by
        // the engine at all, not even to check whether it is a mode-switch
        // key. The only ways back out of Direct mode are `SetInputScope`
        // (scope no longer `Password`) and, once M0's IME-on/off actions
        // are reachable from a key map, the ones in `apply_action`.
        out.consumed = false;
        return Ok(());
    }

    let state = session.state();
    match keymap.lookup(state, key) {
        Some(action) => apply_action(session, action, table, normalizer, scratch, out)?,
        None if session.mode == Mode::FullAlnum || session.mode == Mode::HalfAlnum => {
            apply_alnum_char(session, normalizer, key, out)?;
        }
        None => match key.ch {
            Some(ch) => {
                out.consumed = true;
                table.feed(&mut session.romaji, ch, &mut session.preedit)?;
            }
            None => out.consumed = false,
        },
    }

    render_preedit(session, normalizer, scratch, out)
}

/// Handles a key map action. M0 implements the small subset it has real
/// behaviour for; everything else is swallowed (`consumed = true`, no state
/// change) rather than passed through, so a stray Space or Tab mid-
/// composition cannot land in the host document underneath an active
/// preedit -- see this module's docs on the clean seam that leaves for
/// later phases.
fn apply_action(
    session: &mut Session,
    action: Action,
    table: &Table,
    normalizer: &Normalizer,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    out.consumed = true;
    match action {
        Action::ImeToggle => {
            let mode = if session.mode == Mode::Direct {
                Mode::Hiragana
            } else {
                Mode::Direct
            };
            switch_mode(session, table, normalizer, scratch, mode, out)?;
        }
        Action::ImeOn => switch_mode(session, table, normalizer, scratch, Mode::Hiragana, out)?,
        Action::ImeOff => switch_mode(session, table, normalizer, scratch, Mode::Direct, out)?,
        Action::ModeHiragana => {
            switch_mode(session, table, normalizer, scratch, Mode::Hiragana, out)?;
        }
        Action::ModeKatakana => {
            switch_mode(session, table, normalizer, scratch, Mode::Katakana, out)?;
        }
        Action::ModeHalfKatakana => {
            switch_mode(session, table, normalizer, scratch, Mode::HalfKatakana, out)?;
        }
        Action::ModeFullAlnum => {
            switch_mode(session, table, normalizer, scratch, Mode::FullAlnum, out)?;
        }
        Action::ModeHalfAlnum => {
            switch_mode(session, table, normalizer, scratch, Mode::HalfAlnum, out)?;
        }
        Action::ModeDirect => switch_mode(session, table, normalizer, scratch, Mode::Direct, out)?,
        Action::ModeKanaToggle | Action::ModeKanaCycle => {
            // M0 ships no glyph-level katakana transform (see the module
            // docs), so Hiragana <-> Katakana is the only distinct switch
            // available to toggle or cycle between; this is a seam, not the
            // final behaviour -- once Katakana/HalfKatakana actually
            // transform text, this is where a third state joins the cycle.
            let next = if session.mode == Mode::Hiragana {
                Mode::Katakana
            } else {
                Mode::Hiragana
            };
            switch_mode(session, table, normalizer, scratch, next, out)?;
        }
        Action::Commit => commit_pending(session, table, normalizer, scratch, out)?,
        Action::Cancel => session.reset(),
        Action::DeleteBack => apply_backspace(session),
        _ => {}
    }
    Ok(())
}

/// Tries to delete pending romaji first, falling back to the last resolved
/// kana character only once nothing is pending -- the required "Backspace
/// removes pending romaji first, then emitted kana" behaviour.
fn apply_backspace(session: &mut Session) {
    if !session.romaji.backspace() {
        session.preedit.pop_char();
    }
}

/// Flushes any trailing romaji, normalizes the resolved composition, and
/// commits it -- or does nothing if there was no composition in progress.
///
/// Used for both `Action::Commit` (Enter mid-composition) and the top-level
/// `Request::Commit`/`Request::Revert`... no -- `Revert` never commits (see
/// `Dispatcher::revert`); this is `Commit`'s and `Action::Commit`'s shared
/// path, plus every mode switch (`switch_mode`), which must not silently
/// drop a composition in progress just because the user pressed a mode key
/// instead of Enter (DESIGN 1: never lose user text).
fn commit_pending(
    session: &mut Session,
    table: &Table,
    normalizer: &Normalizer,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    if !session.is_composing() {
        return Ok(());
    }
    // Resolve whatever romaji is still pending as if no more input were
    // coming (a half-typed sokuon consonant, a lone waiting "n") so it
    // commits instead of silently vanishing.
    table.flush(&mut session.romaji, &mut session.preedit)?;
    scratch.clear();
    normalizer.normalize_into(session.preedit.as_str(), session.mode, scratch)?;
    if !scratch.is_empty() {
        out.set_commit(scratch.as_str())?;
    }
    session.reset();
    Ok(())
}

/// Commits any pending composition, then switches to `mode`.
fn switch_mode(
    session: &mut Session,
    table: &Table,
    normalizer: &Normalizer,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    mode: Mode,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    commit_pending(session, table, normalizer, scratch, out)?;
    session.mode = mode;
    out.mode = Some(mode);
    Ok(())
}

/// `Mode::HalfAlnum`/`Mode::FullAlnum`: no preedit ever forms here (DESIGN
/// 5.6's width normalizer is the choke point, applied per keystroke, right
/// before the character leaves the engine) -- each keystroke normalizes and
/// commits immediately.
fn apply_alnum_char(
    session: &Session,
    normalizer: &Normalizer,
    key: &KeyInput,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    match key.ch {
        Some(c) => {
            out.consumed = true;
            let mapped = normalizer.normalize_char(c, session.mode);
            let mut buf = [0u8; 4];
            out.set_commit(mapped.encode_utf8(&mut buf))
        }
        None => {
            out.consumed = false;
            Ok(())
        }
    }
}

/// Rebuilds `out`'s preedit fields from `session`'s current composition, or
/// leaves them empty if nothing is composing (an empty `OutputBuf.preedit`
/// is exactly the "hide the preedit" signal a caller needs after a commit
/// or a cancel).
///
/// The composition is shown as (up to) two segments: the resolved kana
/// (`session.preedit`, normalized fresh into `scratch` -- DESIGN 5.6's
/// choke point, applied here rather than stored normalized, so that a
/// normalizer swapped mid-connection, or one applied twice by a future
/// change, could never double-widen anything) followed by any trailing
/// romaji not yet resolved to kana, shown as typed (real IMEs show
/// in-progress romaji as plain half-width ASCII, not run through the width
/// policy meant for committed alnum text).
fn render_preedit(
    session: &Session,
    normalizer: &Normalizer,
    scratch: &mut FixedStr<MAX_PREEDIT_BYTES>,
    out: &mut OutputBuf,
) -> Result<(), Overflow> {
    if !session.is_composing() {
        return Ok(());
    }
    scratch.clear();
    normalizer.normalize_into(session.preedit.as_str(), session.mode, scratch)?;
    let pending = session.romaji.pending();

    out.begin_preedit();
    if !scratch.is_empty() {
        out.push_segment(scratch.as_str(), UnderlineKind::Raw)?;
    }
    if !pending.is_empty() {
        out.push_segment(pending, UnderlineKind::Raw)?;
    }
    let cursor = scratch.as_str().chars().count() + pending.chars().count();
    out.set_cursor(cursor as u32);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_core::keymap::KeyMap;
    use sakura_core::width::{PunctuationStyle, Width, WidthPolicy};
    use sakura_proto::{KeyCode, Modifiers};

    fn builtin_dispatcher() -> Dispatcher {
        Dispatcher::new().expect("the shipped defaults must compile")
    }

    fn char_key(c: char) -> KeyInput {
        KeyInput {
            code: KeyCode::Char,
            ch: Some(c),
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: false,
        }
    }

    fn test_only_char_key(c: char) -> KeyInput {
        KeyInput {
            test_only: true,
            ..char_key(c)
        }
    }

    fn named_key(code: KeyCode) -> KeyInput {
        KeyInput {
            code,
            ch: None,
            modifiers: Modifiers::NONE,
            repeat: false,
            test_only: false,
        }
    }

    fn create_session(dispatcher: &mut Dispatcher, out: &mut OutputBuf, name: &str) -> SessionId {
        match dispatcher.dispatch(
            &Request::CreateSession {
                process_name: name.to_string(),
            },
            out,
        ) {
            Reply::Message(Response::SessionCreated { session }) => session,
            other => panic!("expected SessionCreated, got {other:?}"),
        }
    }

    fn type_word(dispatcher: &mut Dispatcher, session: SessionId, word: &str, out: &mut OutputBuf) {
        for c in word.chars() {
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: char_key(c),
                },
                out,
            );
        }
    }

    #[test]
    fn typing_konnichiha_produces_the_hiragana_preedit() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        type_word(&mut dispatcher, session, "konnichiha", &mut out);

        assert_eq!(out.preedit_text(), "こんにちは");
    }

    #[test]
    fn enter_commits_the_composition_and_leaves_the_preedit_empty() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "konnichiha", &mut out);

        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Enter),
            },
            &mut out,
        );

        assert_eq!(reply, Reply::Output);
        assert!(out.consumed);
        assert_eq!(out.commit_text(), Some("こんにちは"));
        assert_eq!(out.preedit_text(), "");
    }

    #[test]
    fn escape_cancels_the_composition_without_committing() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "sakura", &mut out);
        assert!(!out.preedit_text().is_empty());

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Escape),
            },
            &mut out,
        );

        assert_eq!(out.commit_text(), None);
        assert_eq!(out.preedit_text(), "");
    }

    #[test]
    fn backspace_removes_pending_romaji_before_emitted_kana() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        // "ka" resolves fully to "か" (nothing pending); the trailing "k"
        // then waits on a vowel.
        type_word(&mut dispatcher, session, "kak", &mut out);
        assert_eq!(out.preedit_text(), "かk");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            "か",
            "first backspace removes the pending romaji"
        );

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::Backspace),
            },
            &mut out,
        );
        assert_eq!(
            out.preedit_text(),
            "",
            "second backspace removes the emitted kana"
        );
    }

    #[test]
    fn a_test_only_key_reports_what_the_real_dispatch_would_without_mutating_the_session() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");

        // "z" has no romaji reading and passes through raw, immediately and
        // deterministically, with no waiting -- ideal for proving a probe
        // did or did not leave a mark.
        let probe_reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: test_only_char_key('z'),
            },
            &mut out,
        );
        assert_eq!(probe_reply, Reply::Output);
        let probe_consumed = out.consumed;

        // If the probe above had mutated the session, this real "z" would
        // land after a leftover "z" and produce "zz" instead of "z".
        let real_reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('z'),
            },
            &mut out,
        );
        assert_eq!(real_reply, Reply::Output);
        assert_eq!(
            out.consumed, probe_consumed,
            "test_only must report the same consumed as the real dispatch"
        );
        assert_eq!(out.preedit_text(), "z");
    }

    #[test]
    fn an_unknown_session_id_is_rejected() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();

        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session: 9999,
                key: char_key('a'),
            },
            &mut out,
        );

        assert_eq!(
            reply,
            Reply::Message(Response::Error(ErrorCode::UnknownSession))
        );
    }

    #[test]
    fn session_ids_are_never_reused_after_deletion() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let first = create_session(&mut dispatcher, &mut out, "a.exe");

        let deleted = dispatcher.dispatch(&Request::DeleteSession { session: first }, &mut out);
        assert_eq!(deleted, Reply::Message(Response::Ok));

        let second = create_session(&mut dispatcher, &mut out, "b.exe");
        assert_ne!(first, second);
        assert!(second > first);
    }

    #[test]
    fn the_session_table_reports_busy_once_full() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        for _ in 0..crate::session::MAX_SESSIONS {
            create_session(&mut dispatcher, &mut out, "app.exe");
        }

        let reply = dispatcher.dispatch(
            &Request::CreateSession {
                process_name: "one-too-many.exe".to_string(),
            },
            &mut out,
        );

        assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::Busy)));
    }

    #[test]
    fn hello_with_the_wrong_version_is_rejected() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();

        let reply = dispatcher.dispatch(
            &Request::Hello {
                client_version: PROTOCOL_VERSION + 1,
            },
            &mut out,
        );

        assert_eq!(
            reply,
            Reply::Message(Response::Error(ErrorCode::UnsupportedVersion))
        );
    }

    #[test]
    fn hello_with_the_right_version_is_accepted() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();

        let reply = dispatcher.dispatch(
            &Request::Hello {
                client_version: PROTOCOL_VERSION,
            },
            &mut out,
        );

        assert_eq!(
            reply,
            Reply::Message(Response::Hello {
                server_version: PROTOCOL_VERSION,
                engine_version: ENGINE_VERSION
            })
        );
    }

    #[test]
    fn set_input_scope_password_forces_direct_mode_and_builds_no_preedit() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "login.exe");
        type_word(&mut dispatcher, session, "ka", &mut out);
        assert_eq!(
            out.preedit_text(),
            "か",
            "sanity check: normally composes before the scope change"
        );

        let reply = dispatcher.dispatch(
            &Request::SetInputScope {
                session,
                scope: InputScope::Password,
            },
            &mut out,
        );
        assert_eq!(reply, Reply::Message(Response::Ok));

        let key_reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('a'),
            },
            &mut out,
        );
        assert_eq!(key_reply, Reply::Output);
        assert!(!out.consumed, "Direct mode must not intercept keys");
        assert_eq!(out.preedit_text(), "");
        assert_eq!(out.commit_text(), None);
    }

    #[test]
    fn half_alnum_types_ascii_straight_through_and_full_alnum_widens_it() {
        // The shipped MS-IME preset binds no key to a direct mode-switch
        // action, so reaching HalfAlnum/FullAlnum needs a key map that does;
        // and the default normalizer never widens anything in any mode (by
        // design -- see `sakura_core::width`'s docs), so reaching a widened
        // FullAlnum result needs a normalizer told to follow the mode.
        let table = Table::builtin().expect("builtin table compiles");
        let keymap =
            KeyMap::parse("[global]\nf2 = \"mode_half_alnum\"\nf3 = \"mode_full_alnum\"\n")
                .expect("small keymap compiles");
        let normalizer = Normalizer {
            width: WidthPolicy {
                alnum: Width::FollowMode,
                number: Width::FollowMode,
                symbol: Width::FollowMode,
            },
            punctuation: PunctuationStyle::default(),
        };
        let mut dispatcher = Dispatcher::with_parts(table, keymap, normalizer);
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "cmd.exe");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::F3),
            },
            &mut out,
        );
        let mut widened = String::new();
        for c in "docker".chars() {
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: char_key(c),
                },
                &mut out,
            );
            assert_eq!(out.preedit_text(), "", "alnum modes never build a preedit");
            widened.push_str(out.commit_text().unwrap_or_default());
        }
        assert_eq!(widened, "ｄｏｃｋｅｒ");

        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: named_key(KeyCode::F2),
            },
            &mut out,
        );
        let mut plain = String::new();
        for c in "docker".chars() {
            dispatcher.dispatch(
                &Request::SendKey {
                    session,
                    key: char_key(c),
                },
                &mut out,
            );
            plain.push_str(out.commit_text().unwrap_or_default());
        }
        assert_eq!(plain, "docker");
    }

    #[test]
    fn an_oversized_preedit_answers_too_large_and_leaves_the_session_usable() {
        // A single entry whose output alone exceeds MAX_PREEDIT_BYTES, so
        // one keystroke overflows deterministically. This table maps
        // nothing but "a", so "usable afterward" is checked with a
        // character this same minimal table can actually resolve: "k" has
        // no entry either, so it passes through raw immediately.
        let huge = "あ".repeat(600); // 1800 bytes > MAX_PREEDIT_BYTES (1536)
        let table = Table::parse(&format!("[kana]\na = \"{huge}\"\n")).expect("table compiles");
        let keymap = KeyMap::preset(Preset::MsIme).expect("preset compiles");
        let mut dispatcher = Dispatcher::with_parts(table, keymap, Normalizer::default());
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "app.exe");

        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('a'),
            },
            &mut out,
        );

        assert_eq!(reply, Reply::Message(Response::Error(ErrorCode::TooLarge)));

        // The session must still be usable afterward, on the same table.
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('k'),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "k");
    }

    #[test]
    fn commit_request_flushes_pending_romaji_instead_of_dropping_it() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        // A lone trailing "n" never resolves through `feed` alone -- it
        // needs a flush, which is exactly what a top-level Commit must do
        // rather than silently drop it.
        type_word(&mut dispatcher, session, "kon", &mut out);
        assert_eq!(out.preedit_text(), "こn");

        let reply = dispatcher.dispatch(&Request::Commit { session }, &mut out);

        assert_eq!(reply, Reply::Output);
        assert_eq!(out.commit_text(), Some("こん"));
        assert_eq!(out.preedit_text(), "");
    }

    #[test]
    fn revert_request_discards_the_composition_without_committing() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let session = create_session(&mut dispatcher, &mut out, "notepad.exe");
        type_word(&mut dispatcher, session, "ka", &mut out);

        let reply = dispatcher.dispatch(&Request::Revert { session }, &mut out);

        assert_eq!(reply, Reply::Message(Response::Ok));
        // Revert answers Ok directly (no Output), so a following SendKey
        // starting fresh is what proves the composition is actually gone.
        dispatcher.dispatch(
            &Request::SendKey {
                session,
                key: char_key('k'),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "k");
    }

    #[test]
    fn ping_is_answered_with_pong() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        assert_eq!(
            dispatcher.dispatch(&Request::Ping, &mut out),
            Reply::Message(Response::Pong)
        );
    }

    #[test]
    fn shutdown_is_answered_but_left_for_the_caller_to_act_on() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        assert_eq!(
            dispatcher.dispatch(&Request::Shutdown, &mut out),
            Reply::Shutdown(Response::Ok)
        );
    }

    #[test]
    fn reset_drops_every_session_but_keeps_configuration() {
        let mut dispatcher = builtin_dispatcher();
        let mut out = OutputBuf::new();
        let first = create_session(&mut dispatcher, &mut out, "a.exe");

        dispatcher.reset();

        let reply = dispatcher.dispatch(
            &Request::SendKey {
                session: first,
                key: char_key('a'),
            },
            &mut out,
        );
        assert_eq!(
            reply,
            Reply::Message(Response::Error(ErrorCode::UnknownSession))
        );

        // Configuration survives: a new session still composes normally.
        let second = create_session(&mut dispatcher, &mut out, "b.exe");
        dispatcher.dispatch(
            &Request::SendKey {
                session: second,
                key: char_key('k'),
            },
            &mut out,
        );
        dispatcher.dispatch(
            &Request::SendKey {
                session: second,
                key: char_key('a'),
            },
            &mut out,
        );
        assert_eq!(out.preedit_text(), "か");
    }
}
