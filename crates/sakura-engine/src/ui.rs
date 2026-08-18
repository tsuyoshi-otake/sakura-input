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

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use sakura_proto::types::CandidatePresentation;
use sakura_proto::{
    AppearanceTheme, Candidate, CandidateDetail, CandidateKind, CandidateList, FixedStr, FixedVec,
    Mode, OutputBuf, Revision, ScreenRect, SessionId, UiState, CANDIDATE_PAGE_SIZE, MAX_CANDIDATES,
    MAX_CANDIDATE_DETAIL_DEFINITION_BYTES, MAX_CANDIDATE_DETAIL_READING_BYTES,
    MAX_CANDIDATE_DETAIL_RELATIONS, MAX_CANDIDATE_DETAIL_RELATION_BYTES, MAX_CANDIDATE_TEXT_BYTES,
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
    history_reading_start: u32,
    history_reading_len: u32,
    history_surface_start: u32,
    history_surface_len: u32,
}

/// Candidate data in the board's allocation-free representation.
#[derive(Debug)]
struct CandidateSnapshot {
    kind: CandidateKind,
    presentation: CandidatePresentation,
    text: FixedStr<MAX_CANDIDATE_TEXT_BYTES>,
    annotations: FixedStr<MAX_CANDIDATE_TEXT_BYTES>,
    history_readings: FixedStr<MAX_CANDIDATE_TEXT_BYTES>,
    history_surfaces: FixedStr<MAX_CANDIDATE_TEXT_BYTES>,
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
            history_readings: FixedStr::new(),
            history_surfaces: FixedStr::new(),
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
                    let history_matches = match (
                        self.history_identity(index),
                        output.candidate_history_identity(index),
                    ) {
                        (None, None) => true,
                        (Some((reading, surface)), Some((output_reading, output_surface))) => {
                            reading == output_reading && surface == output_surface
                        }
                        _ => false,
                    };
                    history_matches
                        && Self::slice(self.text.as_str(), span.text_start, span.text_len) == text
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
            let history_reading_start = self.history_readings.len();
            let history_surface_start = self.history_surfaces.len();
            let history = output.candidate_history_identity(index);
            if self.text.push_str(text).is_err()
                || self.annotations.push_str(annotation).is_err()
                || history
                    .is_some_and(|(reading, _)| self.history_readings.push_str(reading).is_err())
                || history
                    .is_some_and(|(_, surface)| self.history_surfaces.push_str(surface).is_err())
                || self
                    .spans
                    .push(CandidateSpan {
                        text_start: text_start as u32,
                        text_len: text.len() as u32,
                        annotation_start: annotation_start as u32,
                        annotation_len: annotation.len() as u32,
                        history_reading_start: history_reading_start as u32,
                        history_reading_len: history.map_or(0, |(reading, _)| reading.len()) as u32,
                        history_surface_start: history_surface_start as u32,
                        history_surface_len: history.map_or(0, |(_, surface)| surface.len()) as u32,
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
        self.history_readings.clear();
        self.history_surfaces.clear();
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

    fn history_identity(&self, index: usize) -> Option<(&str, &str)> {
        let span = *self.spans.as_slice().get(index)?;
        if span.history_reading_len == 0 || span.history_surface_len == 0 {
            return None;
        }
        let reading = Self::slice(
            self.history_readings.as_str(),
            span.history_reading_start,
            span.history_reading_len,
        );
        let surface = Self::slice(
            self.history_surfaces.as_str(),
            span.history_surface_start,
            span.history_surface_len,
        );
        (!reading.is_empty() && !surface.is_empty()).then_some((reading, surface))
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
                deletable_history: span.history_reading_len != 0 && span.history_surface_len != 0,
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

/// Board-owned selected-detail snapshot. This mirrors one `OutputBuf` detail
/// in fixed storage, so publishing a candidate list never allocates and a UI
/// revision cannot combine candidates from one output with detail from another.
#[derive(Debug)]
struct CandidateDetailSnapshot {
    reading: FixedStr<MAX_CANDIDATE_DETAIL_READING_BYTES>,
    definition: FixedStr<MAX_CANDIDATE_DETAIL_DEFINITION_BYTES>,
    definition_truncated: bool,
    aliases: DetailTermsSnapshot,
    related: DetailTermsSnapshot,
    similar: DetailTermsSnapshot,
    antonyms: DetailTermsSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct DetailTextSpan {
    start: u16,
    len: u16,
}

#[derive(Debug)]
struct DetailTermsSnapshot {
    text: FixedStr<{ MAX_CANDIDATE_DETAIL_RELATION_BYTES * MAX_CANDIDATE_DETAIL_RELATIONS }>,
    spans: FixedVec<DetailTextSpan, MAX_CANDIDATE_DETAIL_RELATIONS>,
}

impl DetailTermsSnapshot {
    fn new() -> Self {
        Self {
            text: FixedStr::new(),
            spans: FixedVec::new(),
        }
    }

    fn clear(&mut self) {
        self.text.clear();
        self.spans.clear();
    }

    fn copy_from<'a>(&mut self, terms: impl Iterator<Item = &'a str>) -> bool {
        self.clear();
        for term in terms {
            let start = self.text.len();
            if self.text.push_str(term).is_err()
                || self
                    .spans
                    .push(DetailTextSpan {
                        start: u16::try_from(start).unwrap_or(u16::MAX),
                        len: u16::try_from(term.len()).unwrap_or(u16::MAX),
                    })
                    .is_err()
            {
                self.clear();
                return false;
            }
        }
        true
    }

    fn terms(&self) -> impl ExactSizeIterator<Item = &str> {
        self.spans.as_slice().iter().map(|span| {
            let start = usize::from(span.start);
            self.text
                .as_str()
                .get(start..start + usize::from(span.len))
                .unwrap_or("")
        })
    }
}

impl CandidateDetailSnapshot {
    fn new() -> Self {
        Self {
            reading: FixedStr::new(),
            definition: FixedStr::new(),
            definition_truncated: false,
            aliases: DetailTermsSnapshot::new(),
            related: DetailTermsSnapshot::new(),
            similar: DetailTermsSnapshot::new(),
            antonyms: DetailTermsSnapshot::new(),
        }
    }

    fn clear(&mut self) {
        self.reading.clear();
        self.definition.clear();
        self.definition_truncated = false;
        self.aliases.clear();
        self.related.clear();
        self.similar.clear();
        self.antonyms.clear();
    }

    fn copy_from_output(&mut self, output: &OutputBuf) -> bool {
        self.clear();
        let Some(detail) = output.candidate_detail() else {
            return true;
        };
        if self.reading.push_str(detail.reading).is_err()
            || self.definition.push_str(detail.definition).is_err()
            || !self.aliases.copy_from(detail.aliases())
            || !self.related.copy_from(detail.related())
            || !self.similar.copy_from(detail.similar())
            || !self.antonyms.copy_from(detail.antonyms())
        {
            self.clear();
            return false;
        }
        self.definition_truncated = detail.definition_truncated;
        true
    }

    fn matches_output(&self, output: &OutputBuf) -> bool {
        let Some(detail) = output.candidate_detail() else {
            return self.reading.is_empty();
        };
        self.reading.as_str() == detail.reading
            && self.definition.as_str() == detail.definition
            && self.definition_truncated == detail.definition_truncated
            && self.aliases.terms().eq(detail.aliases())
            && self.related.terms().eq(detail.related())
            && self.similar.terms().eq(detail.similar())
            && self.antonyms.terms().eq(detail.antonyms())
    }

    fn to_owned(&self) -> Option<CandidateDetail> {
        (!self.reading.is_empty()).then(|| CandidateDetail {
            reading: self.reading.as_str().to_owned(),
            definition: self.definition.as_str().to_owned(),
            definition_truncated: self.definition_truncated,
            aliases: self.aliases.terms().map(str::to_owned).collect(),
            related: self.related.terms().map(str::to_owned).collect(),
            similar: self.similar.terms().map(str::to_owned).collect(),
            antonyms: self.antonyms.terms().map(str::to_owned).collect(),
        })
    }
}

#[derive(Debug)]
struct UiSnapshot {
    revision: Revision,
    appearance_theme: AppearanceTheme,
    mode: Option<Mode>,
    has_candidates: bool,
    candidates: CandidateSnapshot,
    candidate_detail: CandidateDetailSnapshot,
    candidate_session: Option<SessionId>,
    /// The process-wide learning generation used for this prediction list.
    /// A successful history deletion advances it only after durable publish;
    /// every older UI list is then stale and must disappear rather than show a
    /// candidate that was already removed from disk.
    candidate_learning_generation: u64,
    anchor: Option<ScreenRect>,
    document: Option<ScreenRect>,
    renderer_visible: bool,
    stopping: bool,
}

impl UiSnapshot {
    fn initial(appearance_theme: AppearanceTheme) -> Self {
        Self {
            revision: 1,
            appearance_theme,
            mode: None,
            has_candidates: false,
            candidates: CandidateSnapshot::new(),
            candidate_detail: CandidateDetailSnapshot::new(),
            candidate_session: None,
            candidate_learning_generation: 0,
            anchor: None,
            document: None,
            renderer_visible: false,
            stopping: false,
        }
    }

    fn to_owned(&self) -> UiState {
        UiState {
            revision: self.revision,
            appearance_theme: self.appearance_theme,
            mode: self.mode,
            candidates: self.has_candidates.then(|| self.candidates.to_owned()),
            candidate_detail: self
                .has_candidates
                .then(|| self.candidate_detail.to_owned())
                .flatten(),
            anchor: self.anchor,
            document: self.document,
            renderer_visible: self.renderer_visible,
            stopping: self.stopping,
        }
    }
}

/// The current UI state, and a way to wait for the next one.
#[derive(Debug)]
pub struct UiBoard {
    /// Last successfully published process-wide preference. This is separate
    /// from `state` only so the poisoned-lock fallback can still describe the
    /// last known-good palette without guessing from the Windows theme.
    appearance_theme: AtomicU8,
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
        Self::with_appearance_theme(AppearanceTheme::Auto)
    }

    /// A board whose every state carries the configured global appearance.
    /// App profiles deliberately do not participate: popup appearance is a
    /// process-wide renderer setting, not per-document input behavior.
    pub fn with_appearance_theme(appearance_theme: AppearanceTheme) -> Self {
        UiBoard {
            appearance_theme: AtomicU8::new(appearance_theme as u8),
            state: Mutex::new(UiSnapshot::initial(appearance_theme)),
            changed: Condvar::new(),
            delivering: Mutex::new(0),
            quiet: Condvar::new(),
        }
    }

    /// Replaces the global popup appearance after a validated configuration
    /// reload. A no-op does not advance the revision, while a real change is
    /// published even if no candidate popup is currently visible so a later
    /// popup cannot use the old palette.
    ///
    /// Returns `true` only when a new state was published. A poisoned state
    /// lock is retained unchanged: configuration reload must not turn a UI
    /// recovery path into a guessed theme change.
    pub fn set_appearance_theme(&self, appearance_theme: AppearanceTheme) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.appearance_theme == appearance_theme {
            return false;
        }
        state.revision = state.revision.wrapping_add(1);
        state.appearance_theme = appearance_theme;
        self.appearance_theme
            .store(appearance_theme as u8, Ordering::Release);
        drop(state);
        self.changed.notify_all();
        true
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
    pub fn publish_output(&self, session: SessionId, output: &OutputBuf, learning_generation: u64) {
        let has_candidates = CandidateSnapshot::output_is_valid(output);
        let candidate_session = has_candidates.then_some(session);
        let Ok(mut state) = self.state.lock() else {
            return;
        };

        // Electron delivers the same Space to an idle peer context as well as
        // the live reading. That idle session has no candidates. Clearing the
        // shared board here hid the live popup, so conversion candidates never
        // appeared even though the engine had already converted.
        if !has_candidates && state.candidate_session != Some(session) {
            return;
        }

        let next_mode = output.mode.or(state.mode);
        let changed = state.mode != next_mode
            || state.has_candidates != has_candidates
            || (has_candidates && !state.candidates.matches_output(output))
            || (has_candidates && !state.candidate_detail.matches_output(output))
            || state.candidate_session != candidate_session
            || (has_candidates && state.candidate_learning_generation != learning_generation);
        if !changed {
            return;
        }

        if state.candidate_session != candidate_session || !has_candidates {
            state.anchor = None;
            state.document = None;
            state.renderer_visible = false;
        }
        if has_candidates {
            if !state.candidates.copy_from_output(output) {
                // OutputBuf and CandidateSnapshot have the same capacities,
                // so this indicates an internal invariant violation. Publish
                // a terminal hidden state rather than partial candidate data.
                state.has_candidates = false;
                state.candidate_session = None;
                state.candidate_learning_generation = 0;
                state.anchor = None;
                state.document = None;
                state.renderer_visible = false;
                state.revision = state.revision.wrapping_add(1);
                drop(state);
                self.changed.notify_all();
                return;
            }
            if !state.candidate_detail.copy_from_output(output) {
                // Detail is an optional enhancement. A malformed detail may
                // never hide the authoritative candidate list or disturb its
                // selected row; retain a terminal empty-detail snapshot.
                state.candidate_detail.clear();
            }
        } else {
            state.candidates.clear();
            state.candidate_detail.clear();
            state.candidate_learning_generation = 0;
        }
        state.revision = state.revision.wrapping_add(1);
        state.mode = next_mode;
        state.has_candidates = has_candidates;
        state.candidate_session = candidate_session;
        state.candidate_learning_generation = if has_candidates {
            learning_generation
        } else {
            0
        };
        drop(state);
        self.changed.notify_all();
    }

    /// Updates the popup geometry for the session that owns the current list.
    /// Stale layout callbacks from a former focus owner are ignored.
    pub fn publish_placement(
        &self,
        session: SessionId,
        anchor: Option<ScreenRect>,
        document: Option<ScreenRect>,
        renderer_visible: bool,
    ) -> bool {
        let anchor = anchor.filter(|rect| rect.is_valid());
        let document = document.filter(|rect| rect.is_valid());
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if state.candidate_session != Some(session) {
            return false;
        }
        if state.anchor == anchor
            && state.document == document
            && state.renderer_visible == renderer_visible
        {
            return true;
        }
        state.revision = state.revision.wrapping_add(1);
        state.anchor = anchor;
        state.document = document;
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
        state.candidate_detail.clear();
        state.candidate_session = None;
        state.candidate_learning_generation = 0;
        state.anchor = None;
        state.document = None;
        state.renderer_visible = false;
        drop(state);
        self.changed.notify_all();
    }

    /// Returns the exact durable learning key for a renderer request only
    /// when its revision names the current suggestion snapshot and its index
    /// names an engine-marked history row. The renderer never supplies either
    /// string, so a stale row cannot be redirected to a same-looking candidate.
    pub fn history_candidate_identity(
        &self,
        revision: Revision,
        candidate_index: u16,
    ) -> Option<(String, String)> {
        let state = self.state.lock().ok()?;
        if state.revision != revision
            || !state.has_candidates
            || state.candidates.kind != CandidateKind::Suggestion
        {
            return None;
        }
        let (reading, surface) = state
            .candidates
            .history_identity(usize::from(candidate_index))?;
        Some((reading.to_owned(), surface.to_owned()))
    }

    /// Removes the shared candidate snapshot after a learning operation has
    /// durably advanced `learning_generation`. The generation check is what
    /// keeps a concurrent, already-refreshed list visible while hiding every
    /// pre-delete list (including one published by a different pipe worker).
    pub fn invalidate_stale_prediction_candidates(&self, learning_generation: u64) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        if !state.has_candidates
            || state.candidates.kind != CandidateKind::Suggestion
            || state.candidate_learning_generation == learning_generation
        {
            return false;
        }
        state.revision = state.revision.wrapping_add(1);
        state.has_candidates = false;
        state.candidates.clear();
        state.candidate_detail.clear();
        state.candidate_session = None;
        state.candidate_learning_generation = 0;
        state.anchor = None;
        state.document = None;
        state.renderer_visible = false;
        drop(state);
        self.changed.notify_all();
        true
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
                    appearance_theme: appearance_theme_from_u8(
                        self.appearance_theme.load(Ordering::Acquire),
                    ),
                    mode: None,
                    candidates: None,
                    candidate_detail: None,
                    anchor: None,
                    document: None,
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

fn appearance_theme_from_u8(value: u8) -> AppearanceTheme {
    match value {
        0 => AppearanceTheme::Auto,
        1 => AppearanceTheme::Light,
        2 => AppearanceTheme::Dark,
        _ => AppearanceTheme::Auto,
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
    use sakura_proto::{CandidateDetailInput, OutputBuf, MAX_CANDIDATE_DETAIL_DEFINITION_BYTES};
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

    fn history_suggestion_output() -> OutputBuf {
        let mut output = OutputBuf::new();
        output.begin_suggestions(0, 9).expect("suggestion list");
        output
            .push_history_candidate("表示名", "履歴", "よみ", "永続化された表記")
            .expect("history candidate");
        output
            .push_candidate("通常候補", "一般")
            .expect("ordinary candidate");
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
    fn configured_appearance_theme_is_present_in_every_ui_state() {
        let board = UiBoard::with_appearance_theme(AppearanceTheme::Dark);
        let initial = look(&board, 0);
        assert_eq!(initial.appearance_theme, AppearanceTheme::Dark);

        board.publish(Mode::Hiragana);
        assert_eq!(
            look(&board, initial.revision).appearance_theme,
            AppearanceTheme::Dark
        );
    }

    #[test]
    fn reloaded_appearance_publishes_only_a_real_change() {
        let board = UiBoard::with_appearance_theme(AppearanceTheme::Dark);
        let initial = look(&board, 0);

        assert!(!board.set_appearance_theme(AppearanceTheme::Dark));
        assert!(board.set_appearance_theme(AppearanceTheme::Light));

        let updated = look(&board, initial.revision);
        assert_eq!(updated.appearance_theme, AppearanceTheme::Light);
        assert_ne!(updated.revision, initial.revision);
        assert!(!board.set_appearance_theme(AppearanceTheme::Light));
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
        board.publish_output(7, &output, 0);
        let state = look(&board, 1);
        let candidates = state.candidates.expect("published candidates");
        assert_eq!(candidates.selected, 1);
        assert_eq!(candidates.items[0].text, "候補");
        assert_eq!(candidates.items[1].annotation, "名詞");
        assert_eq!(state.anchor, None);
        assert!(!state.renderer_visible);
    }

    #[test]
    fn rejected_detail_leaves_the_candidate_snapshot_visible_and_selected() {
        let board = UiBoard::new();
        let mut output = candidate_output(1);
        let oversized = "x".repeat(MAX_CANDIDATE_DETAIL_DEFINITION_BYTES + 1);
        assert!(output
            .set_candidate_detail(CandidateDetailInput {
                reading: "かな",
                definition: &oversized,
                definition_truncated: false,
                aliases: &[],
                related: &[],
                similar: &[],
                antonyms: &[],
            })
            .is_err());

        board.publish_output(7, &output, 0);
        let state = look(&board, 1);
        assert_eq!(state.candidates.as_ref().map(|list| list.selected), Some(1));
        assert_eq!(
            state.candidates.as_ref().map(|list| list.items.len()),
            Some(2)
        );
        assert_eq!(state.candidate_detail, None);
    }

    #[test]
    fn selection_change_without_detail_clears_the_prior_detail_atomically() {
        let board = UiBoard::new();
        let mut selected_system_entry = candidate_output(0);
        selected_system_entry
            .set_candidate_detail(CandidateDetailInput {
                reading: "かな",
                definition: "selected system definition",
                definition_truncated: false,
                aliases: &[],
                related: &[],
                similar: &[],
                antonyms: &[],
            })
            .expect("valid detail");
        board.publish_output(7, &selected_system_entry, 0);
        let first = look(&board, 1);
        assert_eq!(
            first
                .candidate_detail
                .as_ref()
                .map(|detail| detail.definition.as_str()),
            Some("selected system definition")
        );

        board.publish_output(7, &candidate_output(1), 0);
        let second = look(&board, first.revision);
        assert_eq!(
            second.candidates.as_ref().map(|list| list.selected),
            Some(1)
        );
        assert_eq!(second.candidate_detail, None);
    }

    #[test]
    fn only_the_candidate_owner_can_move_or_show_the_popup() {
        let board = UiBoard::new();
        board.publish_output(7, &candidate_output(0), 0);
        let after_candidates = look(&board, 1);
        let anchor = ScreenRect {
            left: -200,
            top: 10,
            right: -120,
            bottom: 34,
        };

        assert!(!board.publish_placement(8, Some(anchor), None, true));
        let unchanged = look(&board, 0);
        assert_eq!(unchanged.revision, after_candidates.revision);
        assert_eq!(unchanged.anchor, None);

        assert!(board.publish_placement(7, Some(anchor), None, true));
        let placed = look(&board, after_candidates.revision);
        assert_eq!(placed.anchor, Some(anchor));
        assert!(placed.renderer_visible);
    }

    #[test]
    fn a_terminal_output_clears_candidates_and_placement() {
        let board = UiBoard::new();
        board.publish_output(7, &candidate_output(0), 0);
        let candidates = look(&board, 1);
        board.publish_placement(
            7,
            Some(ScreenRect {
                left: 10,
                top: 20,
                right: 30,
                bottom: 40,
            }),
            None,
            true,
        );
        let placed = look(&board, candidates.revision);

        board.publish_output(7, &OutputBuf::new(), 0);
        let cleared = look(&board, placed.revision);
        assert_eq!(cleared.candidates, None);
        assert_eq!(cleared.anchor, None);
        assert!(!cleared.renderer_visible);
    }

    #[test]
    fn idle_peer_output_does_not_hide_another_session_popup() {
        let board = UiBoard::new();
        board.publish_output(7, &history_suggestion_output(), 0);
        let candidates = look(&board, 1);
        let anchor = ScreenRect {
            left: 10,
            top: 20,
            right: 30,
            bottom: 40,
        };
        assert!(board.publish_placement(7, Some(anchor), None, true));
        let placed = look(&board, candidates.revision);

        board.publish_output(8, &OutputBuf::new(), 0);
        let unchanged = look(&board, 0);
        assert_eq!(unchanged.revision, placed.revision);
        assert!(unchanged.candidates.is_some());
        assert_eq!(unchanged.anchor, Some(anchor));
        assert!(unchanged.renderer_visible);

        board.publish_output(7, &candidate_output(0), 0);
        let converted = look(&board, placed.revision);
        assert_eq!(
            converted
                .candidates
                .as_ref()
                .map(|list| list.items[0].text.as_str()),
            Some("候補")
        );
        assert_eq!(converted.anchor, Some(anchor));
        assert!(converted.renderer_visible);
    }

    #[test]
    fn history_delete_capability_is_revision_bound_and_never_inferred_from_annotation() {
        let board = UiBoard::new();
        let output = history_suggestion_output();
        board.publish_output(7, &output, 41);
        let published = look(&board, 1);
        let candidates = published.candidates.as_ref().expect("suggestions");
        assert!(candidates.items[0].deletable_history);
        assert!(!candidates.items[1].deletable_history);
        assert_eq!(
            board.history_candidate_identity(published.revision, 0),
            Some(("よみ".to_owned(), "永続化された表記".to_owned()))
        );

        // Exercise every public index around the bounded list. No annotation,
        // surface, or reading chosen by the caller can widen this authority.
        for index in 0..=u16::try_from(MAX_CANDIDATES).expect("bounded") {
            let expected = (index == 0).then(|| ("よみ".to_owned(), "永続化された表記".to_owned()));
            assert_eq!(
                board.history_candidate_identity(published.revision, index),
                expected,
                "index {index} must remain exact"
            );
        }
        assert_eq!(
            board.history_candidate_identity(published.revision.wrapping_add(1), 0),
            None,
            "a renderer snapshot from another revision is never authority"
        );
    }

    #[test]
    fn durable_history_generation_hides_only_stale_prediction_ui_and_is_idempotent() {
        let board = UiBoard::new();
        board.publish_output(7, &history_suggestion_output(), 41);
        let before = look(&board, 1);
        assert!(board.invalidate_stale_prediction_candidates(42));
        let invalidated = look(&board, before.revision);
        assert_eq!(invalidated.candidates, None);
        assert!(!board.invalidate_stale_prediction_candidates(42));

        // A snapshot already made after a different worker observed the
        // durable generation is current and must not be hidden by the old
        // renderer click completing late.
        board.publish_output(7, &history_suggestion_output(), 42);
        let fresh = look(&board, invalidated.revision);
        assert!(!board.invalidate_stale_prediction_candidates(42));
        assert_eq!(look(&board, 0), fresh);
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
