//! What the renderer draws, shared across every connection.
//!
//! This is the one piece of engine state that is deliberately *not*
//! share-nothing (see [`crate::server`]'s module docs, which promised it
//! would arrive as an explicitly synchronized component rather than as a
//! shared mutable engine). It has to be: the mode changes on the connection
//! belonging to whichever application the user is typing in, and the
//! renderer that has to draw it is a different connection entirely.
//!
//! # Why a condition variable and not a channel
//!
//! The renderer asks `Request::WatchUi { since }` and the engine answers
//! when the state has moved past `since` — a long poll (see that request's
//! docs for why it is a long poll at all). A channel would queue every
//! intermediate state and deliver them one at a time; what the renderer
//! wants is the *latest* state, and a user mashing the mode key while the
//! renderer is busy should cost one redraw of the final mode, not one
//! redraw per keypress. A revision-stamped cell plus a condvar collapses
//! runs of changes for free.
//!
//! # Cost on the keystroke path
//!
//! [`UiBoard::publish_output`] takes one short, normally uncontended lock on
//! the keystroke path. Candidate strings are copied into fixed-capacity
//! buffers before that lock is taken; conversion never allocates merely to
//! tell the renderer what changed. Owned strings are created only on the
//! renderer's long-poll thread in [`UiBoard::wait_past`].
//!
//! # Why the board knows about shutdown
//!
//! The last thing the board publishes is that the engine is going away
//! ([`UiBoard::stop`]), and [`UiBoard::settle`] holds the exit open until
//! the watchers have actually written it to their pipes. That is a strange
//! job for a UI cell until you notice that the renderer is the engine's
//! watchdog: it cannot tell "crashed, restart it" from "stopped by the
//! uninstaller, stay dead" by watching the pipe break, and this is the only
//! channel it is listening on. [`sakura_proto::UiState::stopping`] carries
//! the distinction; the two methods here are what make it reliable rather
//! than a race the uninstaller loses sometimes.

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use sakura_proto::types::CandidatePresentation;
use sakura_proto::{
    Candidate, CandidateKind, CandidateList, FixedStr, FixedVec, Mode, OutputBuf, Revision,
    ScreenRect, SessionId, UiState, CANDIDATE_PAGE_SIZE, MAX_CANDIDATES, MAX_CANDIDATE_TEXT_BYTES,
};

/// How long [`UiBoard::wait_past`] blocks before answering with unchanged
/// state.
///
/// This is not how quickly a dead engine is noticed — a dead engine breaks
/// the pipe, and the renderer's read fails at once. It is how quickly a
/// *hung* engine is noticed: one that still holds its handles but has
/// stopped answering. Five seconds trades a wake-up every five seconds in
/// two idle processes against how long a wedged IME can look fine.
pub const HEARTBEAT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct CandidateSpan {
    text_start: u32,
    text_len: u32,
    annotation_start: u32,
    annotation_len: u32,
}

/// Candidate data in the board's allocation-free representation.
#[derive(Debug)]
struct CandidateSnapshot {
    kind: CandidateKind,
    presentation: CandidatePresentation,
    text: FixedStr<MAX_CANDIDATE_TEXT_BYTES>,
    annotations: FixedStr<MAX_CANDIDATE_TEXT_BYTES>,
    spans: FixedVec<CandidateSpan, MAX_CANDIDATES>,
    selected: u16,
    page_size: u16,
}

impl CandidateSnapshot {
    fn new() -> Self {
        Self {
            kind: CandidateKind::Conversion,
            presentation: CandidatePresentation::Compact,
            text: FixedStr::new(),
            annotations: FixedStr::new(),
            spans: FixedVec::new(),
            selected: 0,
            page_size: CANDIDATE_PAGE_SIZE as u16,
        }
    }

    fn output_is_valid(output: &OutputBuf) -> bool {
        if !output.has_candidates() || output.candidate_count() == 0 {
            return false;
        }
        matches!(
            (output.selected_candidate(), output.candidate_page_size()),
            (Some(selected), Some(page_size))
                if page_size > 0 && usize::from(selected) < output.candidate_count()
        )
    }

    fn matches_output(&self, output: &OutputBuf) -> bool {
        if self.spans.len() != output.candidate_count()
            || Some(self.kind) != output.candidate_kind()
            || Some(self.presentation) != output.candidate_presentation()
            || Some(self.selected) != output.selected_candidate()
            || Some(self.page_size) != output.candidate_page_size()
        {
            return false;
        }
        self.spans
            .as_slice()
            .iter()
            .enumerate()
            .all(|(index, span)| {
                output.candidate(index).is_some_and(|(text, annotation)| {
                    Self::slice(self.text.as_str(), span.text_start, span.text_len) == text
                        && Self::slice(
                            self.annotations.as_str(),
                            span.annotation_start,
                            span.annotation_len,
                        ) == annotation
                })
            })
    }

    /// Reuses the board-owned buffers. No large candidate snapshot is ever
    /// returned through the pipe worker's deliberately small stack.
    fn copy_from_output(&mut self, output: &OutputBuf) -> bool {
        let (Some(presentation), Some(selected), Some(page_size)) = (
            output.candidate_presentation(),
            output.selected_candidate(),
            output.candidate_page_size(),
        ) else {
            return false;
        };
        self.clear();
        self.kind = output.candidate_kind().unwrap_or_default();
        self.presentation = presentation;
        self.selected = selected;
        self.page_size = page_size;
        for index in 0..output.candidate_count() {
            let Some((text, annotation)) = output.candidate(index) else {
                self.clear();
                return false;
            };
            let text_start = self.text.len();
            let annotation_start = self.annotations.len();
            if self.text.push_str(text).is_err()
                || self.annotations.push_str(annotation).is_err()
                || self
                    .spans
                    .push(CandidateSpan {
                        text_start: text_start as u32,
                        text_len: text.len() as u32,
                        annotation_start: annotation_start as u32,
                        annotation_len: annotation.len() as u32,
                    })
                    .is_err()
            {
                self.clear();
                return false;
            }
        }
        true
    }

    fn clear(&mut self) {
        self.kind = CandidateKind::Conversion;
        self.presentation = CandidatePresentation::Compact;
        self.text.clear();
        self.annotations.clear();
        self.spans.clear();
        self.selected = 0;
        self.page_size = CANDIDATE_PAGE_SIZE as u16;
    }

    fn slice(source: &str, start: u32, len: u32) -> &str {
        let start = start as usize;
        let Some(end) = start.checked_add(len as usize) else {
            return "";
        };
        source.get(start..end).unwrap_or("")
    }

    fn to_owned(&self) -> CandidateList {
        let mut items = Vec::with_capacity(self.spans.len());
        for span in self.spans.as_slice() {
            items.push(Candidate {
                text: Self::slice(self.text.as_str(), span.text_start, span.text_len).to_owned(),
                annotation: Self::slice(
                    self.annotations.as_str(),
                    span.annotation_start,
                    span.annotation_len,
                )
                .to_owned(),
            });
        }
        CandidateList {
            kind: self.kind,
            presentation: self.presentation,
            items,
            selected: self.selected,
            page_size: self.page_size,
        }
    }
}

#[derive(Debug)]
struct UiSnapshot {
    revision: Revision,
    mode: Option<Mode>,
    has_candidates: bool,
    candidates: CandidateSnapshot,
    candidate_session: Option<SessionId>,
    anchor: Option<ScreenRect>,
    renderer_visible: bool,
    stopping: bool,
}

impl UiSnapshot {
    fn initial() -> Self {
        Self {
            revision: 1,
            mode: None,
            has_candidates: false,
            candidates: CandidateSnapshot::new(),
            candidate_session: None,
            anchor: None,
            renderer_visible: false,
            stopping: false,
        }
    }

    fn to_owned(&self) -> UiState {
        UiState {
            revision: self.revision,
            mode: self.mode,
            candidates: self.has_candidates.then(|| self.candidates.to_owned()),
            anchor: self.anchor,
            renderer_visible: self.renderer_visible,
            stopping: self.stopping,
        }
    }
}

/// The current UI state, and a way to wait for the next one.
#[derive(Debug)]
pub struct UiBoard {
    state: Mutex<UiSnapshot>,
    changed: Condvar,
    /// Watchers that have been handed a state but have not finished
    /// writing it to their pipe.
    ///
    /// Under its own mutex rather than in [`UiBoard::state`], because the
    /// two are waited on by different threads for different reasons and
    /// sharing one lock would make a watcher parked on `changed` block the
    /// exit that is trying to count it. Under a mutex at all — rather than
    /// an atomic — because [`UiBoard::settle`] waits on it through
    /// [`UiBoard::quiet`], and a condvar whose predicate reads state the
    /// notifier changes *outside* the lock loses wakeups: the decrement can
    /// land between `settle` evaluating the predicate and parking, and then
    /// nothing wakes it until the grace expires.
    ///
    /// The lock is only ever held for the increment or the decrement, never
    /// across the pipe write between them, so a renderer that has stopped
    /// reading cannot hold it.
    delivering: Mutex<usize>,
    quiet: Condvar,
}

impl UiBoard {
    /// A board with nothing to show yet.
    ///
    /// `mode: None` because until the user changes mode there is no
    /// indicator to draw: DESIGN 8 specifies the あ/A indicator as
    /// something that appears *on mode change*, not a permanent overlay.
    pub fn new() -> Self {
        UiBoard {
            state: Mutex::new(UiSnapshot::initial()),
            changed: Condvar::new(),
            delivering: Mutex::new(0),
            quiet: Condvar::new(),
        }
    }

    /// The delivery count, readable even if a panic poisoned it.
    ///
    /// A poisoned count is still an accurate count: every writer is one of
    /// the two `±1` sites below, and neither can leave it torn. Refusing to
    /// read it would make the exit path give up on a wait that is working.
    fn delivering(&self) -> MutexGuard<'_, usize> {
        self.delivering
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Records a mode change and wakes everyone waiting for one.
    ///
    /// A repeat of the mode already showing is dropped rather than
    /// published: the revision is what a renderer redraws on, and bumping
    /// it for state that did not change would make the indicator flash for
    /// no reason.
    pub fn publish(&self, mode: Mode) {
        let Ok(mut state) = self.state.lock() else {
            // The lock is poisoned, which means a thread panicked while
            // holding it. Nothing here can repair that, and a mode
            // indicator is not worth propagating a panic over: the engine
            // keeps typing, the indicator goes stale.
            return;
        };
        if state.mode == Some(mode) {
            return;
        }
        state.revision = state.revision.wrapping_add(1);
        state.mode = Some(mode);
        drop(state);
        self.changed.notify_all();
    }

    /// Publishes the mode/candidate portion of one session output.
    ///
    /// Candidate data is copied into fixed buffers, so this method performs
    /// no heap allocation on the keystroke path. A candidate list owned by a
    /// different session resets placement; an output without candidates
    /// explicitly terminates the previous popup state.
    pub fn publish_output(&self, session: SessionId, output: &OutputBuf) {
        let has_candidates = CandidateSnapshot::output_is_valid(output);
        let candidate_session = has_candidates.then_some(session);
        let Ok(mut state) = self.state.lock() else {
            return;
        };

        let next_mode = output.mode.or(state.mode);
        let changed = state.mode != next_mode
            || state.has_candidates != has_candidates
            || (has_candidates && !state.candidates.matches_output(output))
            || state.candidate_session != candidate_session;
        if !changed {
            return;
        }

        if state.candidate_session != candidate_session || !has_candidates {
            state.anchor = None;
            state.renderer_visible = false;
        }
        if has_candidates {
            if !state.candidates.copy_from_output(output) {
                // OutputBuf and CandidateSnapshot have the same capacities,
                // so this indicates an internal invariant violation. Publish
                // a terminal hidden state rather than partial candidate data.
                state.has_candidates = false;
                state.candidate_session = None;
                state.anchor = None;
                state.renderer_visible = false;
                state.revision = state.revision.wrapping_add(1);
                drop(state);
                self.changed.notify_all();
                return;
            }
        } else {
            state.candidates.clear();
        }
        state.revision = state.revision.wrapping_add(1);
        state.mode = next_mode;
        state.has_candidates = has_candidates;
        state.candidate_session = candidate_session;
        drop(state);
        self.changed.notify_all();
    }

    /// Updates the popup geometry for the session that owns the current list.
    /// Stale layout callbacks from a former focus owner are ignored.
    pub fn publish_placement(
        &self,
        session: SessionId,
        anchor: Option<ScreenRect>,
        renderer_visible: bool,
    ) -> bool {
        let anchor = anchor.filter(|rect| rect.is_valid());
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.candidate_session != Some(session) {
            return false;
        }
        if state.anchor == anchor && state.renderer_visible == renderer_visible {
            return true;
        }
        state.revision = state.revision.wrapping_add(1);
        state.anchor = anchor;
        state.renderer_visible = renderer_visible;
        drop(state);
        self.changed.notify_all();
        true
    }

    /// Clears candidate UI owned by `session`, leaving another session's
    /// newer popup untouched. Used by non-`Output` terminal commands such as
    /// revert and session deletion.
    pub fn clear_session(&self, session: SessionId) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.candidate_session != Some(session) {
            return;
        }
        state.revision = state.revision.wrapping_add(1);
        state.has_candidates = false;
        state.candidates.clear();
        state.candidate_session = None;
        state.anchor = None;
        state.renderer_visible = false;
        drop(state);
        self.changed.notify_all();
    }

    /// Announces that the engine is exiting on purpose.
    ///
    /// Sets the flag the renderer's watchdog reads as "stay dead". Unlike
    /// [`UiBoard::publish`] this always bumps the revision, because every
    /// parked watcher has to be released — a watcher whose `since` happens
    /// to equal the current revision is exactly the one that would
    /// otherwise sleep through the shutdown and wake to a broken pipe.
    pub fn stop(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.revision = state.revision.wrapping_add(1);
            state.stopping = true;
        }
        // Notified even if the lock was poisoned: waking a watcher that
        // then reads a stale state is recoverable, leaving it parked is
        // not.
        self.changed.notify_all();
    }

    /// Blocks until every watcher has delivered the state it was handed, or
    /// until `grace` elapses.
    ///
    /// Called once, on the way out, after [`UiBoard::stop`]. Without it the
    /// exit wins the race essentially always: the worker that answered
    /// `Shutdown` signals the main thread and the process tears down in
    /// microseconds, while the watcher it just woke has not been scheduled
    /// yet — so the farewell is written to a pipe belonging to a process
    /// that no longer exists.
    ///
    /// `grace` is a ceiling, not a delay. A renderer that is reading its
    /// pipe releases this in well under a millisecond; the bound is there
    /// so that a renderer which has stopped reading cannot keep the engine
    /// alive, which would turn a stuck UI into an uninstall that hangs.
    pub fn settle(&self, grace: Duration) {
        let outstanding = self.delivering();
        let _ = self
            .quiet
            .wait_timeout_while(outstanding, grace, |outstanding| *outstanding > 0);
    }

    /// Returns the state once its revision differs from `since`, or after
    /// [`HEARTBEAT`], whichever comes first.
    ///
    /// Compares for *inequality*, not "greater than": the revision wraps
    /// (after 2^64 mode changes, which is not a real scenario but is a real
    /// arm to get right), and a renderer holding a revision the engine has
    /// wrapped past should be told the current state, not blocked forever
    /// waiting for a number that already went by. `since: 0` never matches
    /// a live revision — [`UiBoard::new`] starts at 1 — so a renderer that
    /// just connected is answered immediately.
    ///
    /// The returned [`Delivery`] holds [`UiBoard::settle`] open until the
    /// caller has finished writing the state to its pipe, so it must be
    /// kept alive across that write and dropped straight after.
    pub fn wait_past(&self, since: Revision) -> (UiState, Delivery<'_>) {
        // Claimed before the wait rather than after, so that a `stop`
        // landing while this thread is parked still sees a watcher to wait
        // for. Claiming afterwards leaves a window in which `settle` finds
        // nothing outstanding and returns while a watcher is mid-wakeup.
        *self.delivering() += 1;
        let delivery = Delivery { board: self };

        let Ok(state) = self.state.lock() else {
            // Same reasoning as `publish`. Answering with a synthetic
            // state, rather than blocking or panicking, keeps the
            // renderer's long poll a loop instead of a hang.
            return (
                UiState {
                    revision: since,
                    mode: None,
                    candidates: None,
                    anchor: None,
                    renderer_visible: false,
                    stopping: false,
                },
                delivery,
            );
        };
        let (state, _) = self
            .changed
            .wait_timeout_while(state, HEARTBEAT, |state| state.revision == since)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        (state.to_owned(), delivery)
    }
}

/// A watcher that has been handed a state and has not yet delivered it.
///
/// Exists as a guard rather than a pair of calls because the delivery it
/// covers can end early — a pipe write failing returns straight out of the
/// serve loop — and a count that leaks on that path would make every
/// subsequent [`UiBoard::settle`] wait out its full grace period.
#[derive(Debug)]
pub struct Delivery<'a> {
    board: &'a UiBoard,
}

impl Drop for Delivery<'_> {
    fn drop(&mut self) {
        let mut outstanding = self.board.delivering();
        *outstanding -= 1;
        let last = *outstanding == 0;
        // Released before notifying so the thread being woken does not
        // immediately block on the lock it was woken to acquire.
        drop(outstanding);
        if last {
            self.board.quiet.notify_all();
        }
    }
}

impl Default for UiBoard {
    fn default() -> Self {
        UiBoard::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sakura_proto::OutputBuf;
    use std::sync::Arc;
    use std::time::Instant;

    /// What the serve loop does: take the state, finish with it, let go.
    fn look(board: &UiBoard, since: Revision) -> UiState {
        let (state, delivery) = board.wait_past(since);
        drop(delivery);
        state
    }

    fn candidate_output(selected: u16) -> OutputBuf {
        let mut output = OutputBuf::new();
        output
            .begin_candidates(selected, 9)
            .expect("candidate list");
        output.push_candidate("候補", "一般").expect("candidate");
        output.push_candidate("公募", "名詞").expect("candidate");
        output
    }

    #[test]
    fn a_fresh_watcher_is_answered_at_once() {
        let board = UiBoard::new();
        let started = Instant::now();
        let state = look(&board, 0);
        assert!(
            started.elapsed() < HEARTBEAT,
            "revision 0 is nobody's revision and must not block"
        );
        assert_eq!(state.revision, 1);
        assert_eq!(state.mode, None);
        assert!(!state.stopping);
    }

    #[test]
    fn a_mode_change_bumps_the_revision_and_is_visible() {
        let board = UiBoard::new();
        board.publish(Mode::Katakana);
        let state = look(&board, 1);
        assert_eq!(state.mode, Some(Mode::Katakana));
        assert_ne!(state.revision, 1);
    }

    #[test]
    fn candidates_are_owned_only_when_a_watcher_collects_them() {
        let board = UiBoard::new();
        let output = candidate_output(1);
        board.publish_output(7, &output);
        let state = look(&board, 1);
        let candidates = state.candidates.expect("published candidates");
        assert_eq!(candidates.selected, 1);
        assert_eq!(candidates.items[0].text, "候補");
        assert_eq!(candidates.items[1].annotation, "名詞");
        assert_eq!(state.anchor, None);
        assert!(!state.renderer_visible);
    }

    #[test]
    fn only_the_candidate_owner_can_move_or_show_the_popup() {
        let board = UiBoard::new();
        board.publish_output(7, &candidate_output(0));
        let after_candidates = look(&board, 1);
        let anchor = ScreenRect {
            left: -200,
            top: 10,
            right: -120,
            bottom: 34,
        };

        assert!(!board.publish_placement(8, Some(anchor), true));
        let unchanged = look(&board, 0);
        assert_eq!(unchanged.revision, after_candidates.revision);
        assert_eq!(unchanged.anchor, None);

        assert!(board.publish_placement(7, Some(anchor), true));
        let placed = look(&board, after_candidates.revision);
        assert_eq!(placed.anchor, Some(anchor));
        assert!(placed.renderer_visible);
    }

    #[test]
    fn a_terminal_output_clears_candidates_and_placement() {
        let board = UiBoard::new();
        board.publish_output(7, &candidate_output(0));
        let candidates = look(&board, 1);
        board.publish_placement(
            7,
            Some(ScreenRect {
                left: 10,
                top: 20,
                right: 30,
                bottom: 40,
            }),
            true,
        );
        let placed = look(&board, candidates.revision);

        board.publish_output(7, &OutputBuf::new());
        let cleared = look(&board, placed.revision);
        assert_eq!(cleared.candidates, None);
        assert_eq!(cleared.anchor, None);
        assert!(!cleared.renderer_visible);
    }

    #[test]
    fn publishing_the_mode_already_showing_changes_nothing() {
        let board = UiBoard::new();
        board.publish(Mode::Hiragana);
        let first = look(&board, 1);
        board.publish(Mode::Hiragana);
        let again = look(&board, 0);
        assert_eq!(
            first, again,
            "a repeat of the showing mode must not make the indicator flash"
        );
    }

    /// The waiter is asleep, not spinning: it wakes because `publish` woke
    /// it, well inside the heartbeat that would have woken it anyway.
    #[test]
    fn a_waiter_is_woken_by_the_change_it_was_waiting_for() {
        let board = Arc::new(UiBoard::new());
        let watcher = Arc::clone(&board);
        let waiting = std::thread::spawn(move || {
            let started = Instant::now();
            let state = look(&watcher, 1);
            (state, started.elapsed())
        });

        // Long enough that the watcher is parked in `wait_timeout_while`
        // before the change lands, which is the interleaving under test —
        // the other one (change first, then wait) is covered above.
        std::thread::sleep(Duration::from_millis(50));
        board.publish(Mode::HalfAlnum);

        let (state, waited) = waiting.join().expect("the watcher thread");
        assert_eq!(state.mode, Some(Mode::HalfAlnum));
        assert!(
            waited < HEARTBEAT,
            "woken by the heartbeat ({waited:?}) rather than by the change"
        );
    }

    /// Two changes while nobody is looking collapse into one redraw of the
    /// final state, which is the whole reason this is a revisioned cell
    /// rather than a queue.
    #[test]
    fn runs_of_changes_collapse_to_the_latest() {
        let board = UiBoard::new();
        board.publish(Mode::Katakana);
        board.publish(Mode::HalfKatakana);
        board.publish(Mode::FullAlnum);
        let state = look(&board, 1);
        assert_eq!(state.mode, Some(Mode::FullAlnum));
    }

    /// A watcher parked on the current revision — the ordinary steady
    /// state, and the one a plain `publish` would not disturb — must still
    /// be released by `stop`, and must be told why.
    #[test]
    fn stopping_releases_a_watcher_parked_on_the_current_revision() {
        let board = Arc::new(UiBoard::new());
        let watcher = Arc::clone(&board);
        let waiting = std::thread::spawn(move || {
            let started = Instant::now();
            let state = look(&watcher, 1);
            (state, started.elapsed())
        });

        std::thread::sleep(Duration::from_millis(50));
        board.stop();

        let (state, waited) = waiting.join().expect("the watcher thread");
        assert!(
            state.stopping,
            "the watcher was not told the engine is going"
        );
        assert!(
            waited < HEARTBEAT,
            "released by the heartbeat ({waited:?}) rather than by the stop"
        );
    }

    /// The exit waits for the farewell to be delivered. Without this the
    /// engine's own process teardown outruns the watcher it just woke.
    #[test]
    fn settle_waits_for_a_watcher_still_delivering() {
        let board = Arc::new(UiBoard::new());
        let watcher = Arc::clone(&board);
        let holding = Duration::from_millis(120);
        let (claimed_tx, claimed_rx) = std::sync::mpsc::sync_channel(0);

        let delivering = std::thread::spawn(move || {
            let (_, delivery) = watcher.wait_past(0);
            // Synchronize on the state this test actually needs instead of
            // guessing that the new thread was scheduled within a fixed sleep.
            claimed_tx
                .send(())
                .expect("the settle test still owns its receiver");
            // Stands in for the pipe write, which is what `settle` is
            // really waiting to have finished.
            std::thread::sleep(holding);
            drop(delivery);
        });

        claimed_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("the watcher claimed its delivery slot");
        let started = Instant::now();
        board.settle(HEARTBEAT);
        let waited = started.elapsed();

        delivering.join().expect("the delivering thread");
        assert!(
            waited >= Duration::from_millis(50),
            "settle returned after {waited:?} while a watcher was still delivering"
        );
        assert!(
            waited < HEARTBEAT,
            "settle should end when the watcher does"
        );
    }

    /// A renderer that has stopped reading its pipe must not be able to
    /// keep the engine alive — that would turn a stuck UI into an
    /// uninstall that hangs.
    #[test]
    fn settle_gives_up_on_a_watcher_that_never_finishes() {
        let board = UiBoard::new();
        let (_state, stuck) = board.wait_past(0);

        let grace = Duration::from_millis(80);
        let started = Instant::now();
        board.settle(grace);
        let waited = started.elapsed();

        assert!(waited >= grace, "settle returned before its grace expired");
        assert!(
            waited < grace * 10,
            "settle waited {waited:?} on a grace of {grace:?}"
        );
        drop(stuck);
    }

    /// Nothing outstanding means nothing to wait for. The common case at
    /// exit — no renderer running — must not cost the full grace period.
    #[test]
    fn settle_returns_at_once_when_nobody_is_watching() {
        let board = UiBoard::new();
        let started = Instant::now();
        board.settle(HEARTBEAT);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "settle blocked with no watcher outstanding"
        );
    }
}
