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
//! candidate list): a session only ever accumulates a romaji-derived or
//! keyboard-layout kana reading and commits or discards it whole. That means the
//! [`sakura_core::keymap::State`] a session reports is always either
//! [`State::Idle`] or [`State::Composing`] — never `Converting` or
//! `Predicting` — which is deliberate, not a placeholder still to be wired
//! up: those two states do not exist yet because nothing produces them yet.
//!
//! `Mode::Katakana` and `Mode::HalfKatakana` are tracked here (a session can
//! be *in* either mode) but M0 has no glyph-level hiragana → katakana
//! transform anywhere in the workspace (`sakura_core` ships none), so a
//! session in either mode uses either the hiragana-only romaji FSM or the
//! configured direct-layout path. Callers should not read a session reporting
//! `Mode::Katakana` as a promise that its preedit text is katakana yet — see
//! `crate::dispatch`'s module docs for how the dispatcher handles this seam.

use sakura_core::keymap::State;
use sakura_core::romaji;
use sakura_core::{
    resolve_context_preferences, AppProfile, ContextPreferences, ConversionMethod,
    ConversionSegment, InputMethod, Normalizer, Preferences, SegmentTransform, ShiftSpaceBehavior,
    SpaceWidth, SuggestAccept,
};
use sakura_proto::types::CandidatePresentation;
use sakura_proto::{
    FixedStr, FixedVec, InputScope, Mode, SessionId, MAX_COMMIT_BYTES, MAX_PREEDIT_BYTES,
    MAX_SEGMENTS,
};

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

/// Volatile recency window described by DESIGN §5.8.
pub const COMMIT_CACHE_CAPACITY: usize = 8;

/// Whether a host scope requires direct pass-through and a cleared personal
/// context. This predicate is shared by real scope publication, Probe's
/// throwaway transition, and the persistence guard.
pub(crate) const fn scope_is_sensitive(scope: InputScope) -> bool {
    matches!(
        scope,
        InputScope::Password | InputScope::Url | InputScope::Email | InputScope::Digits
    )
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CommitCacheEntry {
    reading_hash: u64,
    reading_len: u16,
    surface_hash: u64,
    surface_len: u16,
    it_words: u8,
    total_words: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UndoRecord {
    reading: FixedStr<MAX_PREEDIT_BYTES>,
    raw_input: FixedStr<MAX_PREEDIT_BYTES>,
    shifted_ascii: bool,
    /// The exact committed surface that must be verified at the host caret
    /// before the frontend may delete it for undo.
    surface: FixedStr<MAX_COMMIT_BYTES>,
    previous_right_id: u16,
    previous_had_carry: bool,
    /// Carry state that was visible after the commit. It is retained so a
    /// host-side rejection can put the engine back at the exact pre-undo
    /// terminal state without spending the one undo record.
    post_commit_right_id: u16,
    post_commit_had_carry: bool,
}

impl Default for UndoRecord {
    fn default() -> Self {
        Self {
            reading: FixedStr::new(),
            raw_input: FixedStr::new(),
            shifted_ascii: false,
            surface: FixedStr::new(),
            previous_right_id: 0,
            previous_had_carry: false,
            post_commit_right_id: 0,
            post_commit_had_carry: false,
        }
    }
}

/// One editing session's state: what mode it is in, what input scope the
/// focused field reported, and the composition (if any) in progress.
///
/// Cloning a `Session` is a fixed-size, allocation-free copy — every field
/// is a `Copy` value or a stack-resident `FixedStr`/[`romaji::Input`] — which
/// is what lets [`crate::dispatch`] answer a `test_only` key event by
/// running the real logic against a clone and discarding it, instead of
/// needing a separate "what would happen" code path to keep in sync with
/// the real one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    process_name: FixedStr<MAX_PROCESS_NAME_BYTES>,
    /// The IME mode new keystrokes are interpreted under.
    pub(crate) mode: Mode,
    /// Width and punctuation policy resolved once from the host profile.
    pub(crate) normalizer: Normalizer,
    /// Space-key policy resolved once from the host profile.
    pub(crate) space_width: SpaceWidth,
    pub(crate) shift_space_behavior: ShiftSpaceBehavior,
    /// Ordinary kana input path resolved once from the host profile.
    pub(crate) input_method: InputMethod,
    /// Whether conversion may produce several bunsetsu segments.
    pub(crate) conversion_method: ConversionMethod,
    /// Prediction policy resolved once from the host profile.
    pub(crate) prediction_enabled: bool,
    pub(crate) suggest_accept: SuggestAccept,
    /// Whether conversion may carry the previous grammatical connection into
    /// the next segment. This is the bounded L1 associative-conversion switch.
    pub(crate) association_enabled: bool,
    /// The input scope of the field this session belongs to (DESIGN 9).
    pub(crate) scope: InputScope,
    /// `true` only after TSF has positively classified the scope. A default
    /// `Normal` value is not enough to permit developer-history persistence.
    pub(crate) scope_classified: bool,
    /// Stable identity used to correlate records across pipe connections.
    history_session_id: SessionId,
    /// The user-selected mode to restore after a sensitive field stops being
    /// focused. Sensitive scopes temporarily force direct pass-through, but a
    /// later scope-read failure or normal field must not strand the session in
    /// direct mode.
    sensitive_mode_restore: Option<Mode>,
    /// Romaji typed but not yet resolved to kana (the FSM's own state).
    pub(crate) romaji: romaji::Input,
    /// Kana (and any unmapped passthrough characters) resolved so far,
    /// *before* the width normalizer runs. `crate::dispatch` normalizes a
    /// copy of this into the wire response; it is never normalized in
    /// place, so that normalizing twice (impossible today, but a change
    /// that made it possible tomorrow) could never double-widen anything.
    pub(crate) preedit: FixedStr<MAX_PREEDIT_BYTES>,
    /// Physical romaji retained for explicit F9/F10 transforms.
    pub(crate) raw_input: FixedStr<MAX_PREEDIT_BYTES>,
    /// True only for a composition that started with a Shift+ASCII letter and
    /// has contained only ASCII characters since. The initial Shift latches
    /// the English composition through its subsequent unshifted ASCII keys.
    /// This is the narrow signal used to try an English dictionary reading;
    /// ordinary romaji remains on the kana path.
    pub(crate) shifted_ascii: bool,
    /// Character cursor in `preedit`; pending romaji is always at this point.
    pub(crate) cursor: u16,
    /// Whether the reading has entered dictionary conversion.
    pub(crate) converting: bool,
    /// Conversion starts compact and only CandidateExpand exposes its page.
    /// This belongs to the session rather than the renderer so output survives
    /// UI reconnection and every conversion terminal path can reset it.
    conversion_presentation: CandidatePresentation,
    /// Monotonic identity for the preedit text used by the prediction worker.
    pub(crate) prediction_generation: u64,
    /// Suggestions may be visible without owning keyboard focus.
    pub(crate) suggestions_visible: bool,
    /// `true` only after Tab/Shift+Tab enters the suggest list.
    pub(crate) suggestion_focused: bool,
    /// Signed so reverse cycling from the first entry wraps naturally.
    pub(crate) suggestion_selection: i16,
    /// Signed until rendering so `CandidatePrev` from zero can mean the last
    /// item without knowing the current candidate count in the key handler.
    pub(crate) selected_candidate: i16,
    /// UTF-8 reading end offsets for every pinned conversion segment.
    segment_ends: FixedVec<u16, MAX_SEGMENTS>,
    /// Candidate selection is independent for each segment.
    segment_selections: [i16; MAX_SEGMENTS],
    segment_transforms: [SegmentTransform; MAX_SEGMENTS],
    segment_transform_cycles: [u8; MAX_SEGMENTS],
    focused_segment: u8,
    /// Grammatical right context carried across commit boundaries.
    carry_right_id: u16,
    has_carry: bool,
    /// Compact, volatile hashes avoid retaining another eight copies of user
    /// text while still giving exact candidate matching with length guards.
    commit_cache: [CommitCacheEntry; COMMIT_CACHE_CAPACITY],
    commit_cache_len: u8,
    commit_cache_next: u8,
    undo_record: UndoRecord,
    undo_armed: bool,
    /// The frontend has an exact-text undo output in flight. While this is
    /// set, key dispatch must not advance the session until the frontend
    /// acknowledges whether the host document deletion was applied.
    undo_pending: bool,
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
            normalizer: Normalizer::default(),
            space_width: SpaceWidth::SameAsInput,
            shift_space_behavior: ShiftSpaceBehavior::Opposite,
            input_method: InputMethod::Romaji,
            conversion_method: ConversionMethod::MultiSegment,
            prediction_enabled: false,
            suggest_accept: SuggestAccept::Disabled,
            association_enabled: true,
            scope: InputScope::Normal,
            scope_classified: false,
            history_session_id: 0,
            sensitive_mode_restore: None,
            romaji: romaji::Input::new(),
            preedit: FixedStr::new(),
            raw_input: FixedStr::new(),
            shifted_ascii: false,
            cursor: 0,
            converting: false,
            conversion_presentation: CandidatePresentation::Compact,
            prediction_generation: 0,
            suggestions_visible: false,
            suggestion_focused: false,
            suggestion_selection: 0,
            selected_candidate: 0,
            segment_ends: FixedVec::new(),
            segment_selections: [0; MAX_SEGMENTS],
            segment_transforms: [SegmentTransform::None; MAX_SEGMENTS],
            segment_transform_cycles: [0; MAX_SEGMENTS],
            focused_segment: 0,
            carry_right_id: 0,
            has_carry: false,
            commit_cache: [CommitCacheEntry::default(); COMMIT_CACHE_CAPACITY],
            commit_cache_len: 0,
            commit_cache_next: 0,
            undo_record: UndoRecord::default(),
            undo_armed: false,
            undo_pending: false,
        }
    }

    /// The host process name this session was created for (possibly
    /// truncated; see [`Session::new`]).
    pub fn process_name(&self) -> &str {
        self.process_name.as_str()
    }

    /// Applies a profile only during context creation. Later refocuses never
    /// call this, so a user-selected mode remains authoritative.
    pub(crate) fn apply_context_preferences(&mut self, preferences: ContextPreferences) {
        self.mode = preferences.default_mode;
        self.input_method = preferences.input_method;
        self.conversion_method = preferences.conversion_method;
        self.normalizer = preferences.normalizer;
        self.space_width = preferences.space_width;
        self.shift_space_behavior = preferences.shift_space_behavior;
        self.prediction_enabled = preferences.prediction_enabled;
        self.suggest_accept = preferences.suggest_accept;
        self.association_enabled = preferences.association_enabled;
    }

    /// The current IME mode.
    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The input scope last reported for this session's field.
    pub fn scope(&self) -> InputScope {
        self.scope
    }

    pub(crate) fn scope_classified(&self) -> bool {
        self.scope_classified
    }

    /// Applies the one authoritative host-scope transition to this session.
    ///
    /// Returns whether the caller must clear its per-session prediction cache.
    /// Probe invokes this on a cloned session and therefore never gets a path
    /// to the live cache; Apply invokes it before clearing the live cache.
    pub(crate) fn apply_input_scope(&mut self, scope: InputScope) -> bool {
        if scope == InputScope::Unclassified {
            let was_sensitive = scope_is_sensitive(self.scope);
            self.scope = InputScope::Normal;
            self.scope_classified = false;
            if was_sensitive {
                self.reset();
                self.clear_personal_context();
                self.restore_mode_after_sensitive();
                return true;
            }
            return false;
        }

        let was_sensitive = scope_is_sensitive(self.scope);
        self.scope = scope;
        self.scope_classified = true;
        if scope_is_sensitive(scope) {
            // Sensitive fields bypass composition entirely. Discarding an
            // earlier reading is intentional: flushing it could leak text
            // typed before the host's scope was known.
            self.remember_mode_before_sensitive();
            self.reset();
            self.clear_personal_context();
            self.mode = Mode::Direct;
            true
        } else if was_sensitive {
            self.reset();
            self.clear_personal_context();
            self.restore_mode_after_sensitive();
            true
        } else {
            false
        }
    }

    pub(crate) fn set_history_session_id(&mut self, id: SessionId) {
        self.history_session_id = id;
    }

    pub(crate) fn history_session_id(&self) -> SessionId {
        self.history_session_id
    }

    pub(crate) fn remember_mode_before_sensitive(&mut self) {
        if self.sensitive_mode_restore.is_none() {
            self.sensitive_mode_restore = Some(self.mode);
        }
    }

    pub(crate) fn restore_mode_after_sensitive(&mut self) {
        if let Some(mode) = self.sensitive_mode_restore.take() {
            self.mode = mode;
        }
    }

    /// The keymap state this session is in.
    ///
    /// M0 has no conversion, so the only two states a session can ever
    /// report are [`State::Idle`] (nothing pending) and [`State::Composing`]
    /// (romaji is pending, or kana has been resolved and not yet committed
    /// or cancelled) — see the module docs.
    pub fn state(&self) -> State {
        if self.converting {
            State::Converting
        } else if self.suggestion_focused {
            State::Predicting
        } else if self.is_composing() {
            State::Composing
        } else {
            State::Idle
        }
    }

    /// `true` if there is a composition in progress: pending romaji, or
    /// kana already resolved from it and not yet committed or cancelled.
    pub fn is_composing(&self) -> bool {
        self.converting
            || !self.romaji.is_empty()
            || !self.preedit.is_empty()
            || (self.shifted_ascii && !self.raw_input.is_empty())
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
        self.raw_input.clear();
        self.shifted_ascii = false;
        self.cursor = 0;
        self.converting = false;
        self.conversion_presentation = CandidatePresentation::Compact;
        self.invalidate_prediction();
        self.selected_candidate = 0;
        self.clear_segments();
    }

    pub(crate) fn cancel_conversion(&mut self) {
        self.converting = false;
        self.conversion_presentation = CandidatePresentation::Compact;
        self.invalidate_prediction();
        self.selected_candidate = 0;
        self.clear_segments();
        self.cursor = u16::try_from(self.preedit.as_str().chars().count()).unwrap_or(u16::MAX);
    }

    pub(crate) fn set_segments(&mut self, segments: &[ConversionSegment]) -> bool {
        self.hide_suggestions();
        self.clear_segments();
        for segment in segments {
            if self.segment_ends.push(segment.reading_end).is_err() {
                self.clear_segments();
                return false;
            }
        }
        !self.segment_ends.is_empty()
    }

    /// Enters a newly begun conversion. Re-entering conversion is deliberately
    /// not folded into this helper: callers that merely move a candidate must
    /// preserve an already expanded presentation.
    pub(crate) fn begin_conversion(&mut self) {
        self.converting = true;
        self.conversion_presentation = CandidatePresentation::Compact;
    }

    /// Changes a live conversion to expanded presentation. Repeating the
    /// action is a successful no-op, while callers receive `false` outside
    /// conversion and can report their recoverable beep outcome.
    pub(crate) fn expand_conversion(&mut self) -> bool {
        if !self.converting {
            return false;
        }
        self.conversion_presentation = CandidatePresentation::Expanded;
        true
    }

    pub(crate) const fn conversion_presentation(&self) -> CandidatePresentation {
        self.conversion_presentation
    }

    pub(crate) fn clear_segments(&mut self) {
        self.segment_ends.clear();
        self.segment_selections.fill(0);
        self.segment_transforms.fill(SegmentTransform::None);
        self.segment_transform_cycles.fill(0);
        self.focused_segment = 0;
    }

    pub(crate) fn invalidate_prediction(&mut self) {
        self.prediction_generation = self.prediction_generation.wrapping_add(1);
        self.suggestions_visible = false;
        self.suggestion_focused = false;
        self.suggestion_selection = 0;
    }

    pub(crate) fn show_suggestions(&mut self, available: bool) {
        self.suggestions_visible = available;
        if !available {
            self.suggestion_focused = false;
            self.suggestion_selection = 0;
        }
    }

    pub(crate) fn hide_suggestions(&mut self) {
        self.suggestions_visible = false;
        self.suggestion_focused = false;
        self.suggestion_selection = 0;
    }

    pub(crate) fn focus_suggestion(&mut self, direction: i16, count: usize) -> bool {
        let Ok(count) = i16::try_from(count) else {
            return false;
        };
        if count == 0 {
            return false;
        }
        self.suggestions_visible = true;
        if self.suggestion_focused {
            self.suggestion_selection = self
                .suggestion_selection
                .saturating_add(direction)
                .rem_euclid(count);
        } else {
            self.suggestion_focused = true;
            self.suggestion_selection = if direction < 0 { count - 1 } else { 0 };
        }
        true
    }

    pub(crate) fn selected_suggestion(&self, count: usize) -> Option<usize> {
        let count = i16::try_from(count).ok()?;
        (count > 0).then(|| self.suggestion_selection.rem_euclid(count) as usize)
    }

    pub(crate) fn segment_count(&self) -> usize {
        self.segment_ends.len()
    }

    pub(crate) fn focused_segment(&self) -> usize {
        usize::from(self.focused_segment).min(self.segment_count().saturating_sub(1))
    }

    pub(crate) fn segment_range(&self, index: usize) -> Option<core::ops::Range<usize>> {
        let end = usize::from(*self.segment_ends.get(index)?);
        let start = if index == 0 {
            0
        } else {
            usize::from(*self.segment_ends.get(index - 1)?)
        };
        (start < end && end <= self.preedit.len()).then_some(start..end)
    }

    pub(crate) fn focus_previous_segment(&mut self) {
        self.focused_segment = self.focused_segment.saturating_sub(1);
        self.selected_candidate = self.segment_selection(self.focused_segment());
    }

    pub(crate) fn focus_next_segment(&mut self) {
        let last = self.segment_count().saturating_sub(1);
        self.focused_segment = u8::try_from((self.focused_segment() + 1).min(last)).unwrap_or(0);
        self.selected_candidate = self.segment_selection(self.focused_segment());
    }

    /// Jumps focus straight to the first segment (Home while converting).
    /// `CaretHome`/`CaretEnd` address the raw preedit cursor, which is not
    /// meaningful once conversion has cut the preedit into segments — this
    /// is the segment-navigation equivalent, filling the gap noted in issue
    /// #16 finding E ("`Action` has no variant for first/last segment").
    pub(crate) fn focus_first_segment(&mut self) {
        self.focused_segment = 0;
        self.selected_candidate = self.segment_selection(self.focused_segment());
    }

    /// Jumps focus straight to the last segment (End while converting).
    pub(crate) fn focus_last_segment(&mut self) {
        let last = self.segment_count().saturating_sub(1);
        self.focused_segment = u8::try_from(last).unwrap_or(0);
        self.selected_candidate = self.segment_selection(self.focused_segment());
    }

    pub(crate) fn segment_selection(&self, index: usize) -> i16 {
        self.segment_selections.get(index).copied().unwrap_or(0)
    }

    pub(crate) fn set_segment_selection(&mut self, index: usize, selection: i16) {
        if let Some(stored) = self.segment_selections.get_mut(index) {
            *stored = selection;
        }
        if index == self.focused_segment() {
            self.selected_candidate = selection;
        }
    }

    pub(crate) fn segment_transform(&self, index: usize) -> (SegmentTransform, u8) {
        (
            self.segment_transforms
                .get(index)
                .copied()
                .unwrap_or_default(),
            self.segment_transform_cycles
                .get(index)
                .copied()
                .unwrap_or(0),
        )
    }

    pub(crate) fn apply_segment_transform(&mut self, transform: SegmentTransform) {
        let index = self.focused_segment();
        let Some(stored) = self.segment_transforms.get_mut(index) else {
            return;
        };
        let Some(cycle) = self.segment_transform_cycles.get_mut(index) else {
            return;
        };
        if *stored == transform {
            *cycle = cycle.wrapping_add(1) % 3;
        } else {
            *stored = transform;
            *cycle = 0;
        }
    }

    pub(crate) fn clear_segment_transform(&mut self, index: usize) {
        if let Some(stored) = self.segment_transforms.get_mut(index) {
            *stored = SegmentTransform::None;
        }
        if let Some(cycle) = self.segment_transform_cycles.get_mut(index) {
            *cycle = 0;
        }
    }

    /// Writes back an exact `(transform, cycle)` pair, as returned earlier by
    /// [`Session::segment_transform`]. Callers use this to undo a speculative
    /// `clear_segment_transform` when the fallible work it was staged for
    /// (e.g. committing a numbered candidate) did not actually go through.
    pub(crate) fn restore_segment_transform(
        &mut self,
        index: usize,
        transform: SegmentTransform,
        cycle: u8,
    ) {
        if let Some(stored) = self.segment_transforms.get_mut(index) {
            *stored = transform;
        }
        if let Some(stored_cycle) = self.segment_transform_cycles.get_mut(index) {
            *stored_cycle = cycle;
        }
    }

    pub(crate) fn carry_right_id(&self) -> u16 {
        if self.has_carry {
            self.carry_right_id
        } else {
            0
        }
    }

    pub(crate) fn reset_carryover(&mut self) {
        self.carry_right_id = 0;
        self.has_carry = false;
    }

    /// Returns the most recently committed surface fingerprint for `reading`.
    /// Lengths accompany the hashes so a collision must agree on both before
    /// it can influence a candidate.
    pub(crate) fn cached_surface_fingerprint(&self, reading: &str) -> Option<(u64, u16)> {
        let reading_len = u16::try_from(reading.len()).ok()?;
        let reading_hash = text_hash(reading);
        let len = usize::from(self.commit_cache_len);
        for distance in 0..len {
            let next = usize::from(self.commit_cache_next);
            let index = (next + COMMIT_CACHE_CAPACITY - 1 - distance) % COMMIT_CACHE_CAPACITY;
            let entry = self.commit_cache[index];
            if entry.reading_len == reading_len && entry.reading_hash == reading_hash {
                return Some((entry.surface_hash, entry.surface_len));
            }
        }
        None
    }

    pub(crate) fn domain_it_ratio_per_mille(&self) -> u16 {
        let mut it_words = 0u32;
        let mut total_words = 0u32;
        let len = usize::from(self.commit_cache_len);
        for distance in 0..len {
            let next = usize::from(self.commit_cache_next);
            let index = (next + COMMIT_CACHE_CAPACITY - 1 - distance) % COMMIT_CACHE_CAPACITY;
            let entry = self.commit_cache[index];
            it_words = it_words.saturating_add(u32::from(entry.it_words));
            total_words = total_words.saturating_add(u32::from(entry.total_words));
        }
        u16::try_from(
            it_words
                .saturating_mul(1_000)
                .checked_div(total_words)
                .unwrap_or(0),
        )
        .unwrap_or(1_000)
    }

    /// Records the composition about to be committed. This must run before
    /// [`Session::reset`] so depth-one undo can restore the physical reading.
    pub(crate) fn record_current_commit(
        &mut self,
        surface: &str,
        right_id: u16,
        it_words: u8,
        total_words: u8,
    ) {
        if matches!(
            self.scope,
            InputScope::Password | InputScope::Url | InputScope::Email | InputScope::Digits
        ) || surface.is_empty()
        {
            self.clear_personal_context();
            return;
        }

        self.undo_record.reading.clear();
        self.undo_record.raw_input.clear();
        self.undo_record.shifted_ascii = self.shifted_ascii;
        self.undo_record.surface.clear();
        if self
            .undo_record
            .reading
            .push_str(self.preedit.as_str())
            .is_err()
            || self
                .undo_record
                .raw_input
                .push_str(self.raw_input.as_str())
                .is_err()
            || self.undo_record.surface.push_str(surface).is_err()
        {
            self.disarm_commit_undo();
            return;
        }
        self.undo_record.previous_right_id = self.carry_right_id;
        self.undo_record.previous_had_carry = self.has_carry;
        self.undo_armed = true;
        self.undo_pending = false;

        let entry = CommitCacheEntry {
            reading_hash: text_hash(self.preedit.as_str()),
            reading_len: u16::try_from(self.preedit.len()).unwrap_or(u16::MAX),
            surface_hash: text_hash(surface),
            surface_len: u16::try_from(surface.len()).unwrap_or(u16::MAX),
            it_words,
            total_words,
        };
        let index = usize::from(self.commit_cache_next);
        self.commit_cache[index] = entry;
        self.commit_cache_next = u8::try_from((index + 1) % COMMIT_CACHE_CAPACITY).unwrap_or(0);
        self.commit_cache_len = self
            .commit_cache_len
            .saturating_add(1)
            .min(COMMIT_CACHE_CAPACITY as u8);

        if right_id == 0 || is_sentence_boundary(surface) {
            self.reset_carryover();
        } else {
            self.carry_right_id = right_id;
            self.has_carry = true;
        }
        self.undo_record.post_commit_right_id = self.carry_right_id;
        self.undo_record.post_commit_had_carry = self.has_carry;
    }

    /// Begins a depth-one commit undo transaction.
    ///
    /// The composition is restored for the output, but the record and recency
    /// entry remain live until the frontend sends an explicit applied or
    /// rejected outcome. This prevents an async TSF validation failure from
    /// leaving the engine restored while the document still contains the
    /// committed surface.
    pub(crate) fn undo_commit(&mut self) -> Option<FixedStr<MAX_COMMIT_BYTES>> {
        if self.undo_pending {
            return None;
        }
        if !self.undo_armed || self.is_composing() {
            self.disarm_commit_undo();
            return None;
        }
        let reading = self.undo_record.reading.clone();
        let raw_input = self.undo_record.raw_input.clone();
        let shifted_ascii = self.undo_record.shifted_ascii;
        let surface = self.undo_record.surface.clone();
        let previous_right_id = self.undo_record.previous_right_id;
        let previous_had_carry = self.undo_record.previous_had_carry;
        if reading.is_empty() || surface.is_empty() {
            self.disarm_commit_undo();
            return None;
        }

        self.undo_pending = true;
        self.carry_right_id = previous_right_id;
        self.has_carry = previous_had_carry;
        self.reset();
        self.preedit = reading;
        self.raw_input = raw_input;
        self.shifted_ascii = shifted_ascii;
        self.cursor = u16::try_from(self.preedit.as_str().chars().count()).unwrap_or(u16::MAX);
        Some(surface)
    }

    pub(crate) fn undo_pending(&self) -> bool {
        self.undo_pending
    }

    /// Commits the engine half after the host has deleted the exact surface.
    /// Only this terminal path consumes the undo record and its recency entry.
    pub(crate) fn acknowledge_undo_commit(&mut self) -> bool {
        if !self.undo_pending {
            return false;
        }
        self.undo_pending = false;
        self.undo_armed = false;
        if self.commit_cache_len > 0 {
            let previous = (usize::from(self.commit_cache_next) + COMMIT_CACHE_CAPACITY - 1)
                % COMMIT_CACHE_CAPACITY;
            self.commit_cache_next = u8::try_from(previous).unwrap_or(0);
            self.commit_cache_len = self.commit_cache_len.saturating_sub(1);
            self.commit_cache[previous] = CommitCacheEntry::default();
        }
        self.clear_undo_record();
        true
    }

    /// Rejects the host-side deletion before any document mutation was made.
    /// Restore the exact post-commit idle state and keep the one undo record so
    /// a later, correctly positioned caret can retry it.
    pub(crate) fn reject_undo_commit(&mut self) -> bool {
        if !self.undo_pending {
            return false;
        }
        let right_id = self.undo_record.post_commit_right_id;
        let had_carry = self.undo_record.post_commit_had_carry;
        self.undo_pending = false;
        self.reset();
        self.carry_right_id = right_id;
        self.has_carry = had_carry;
        true
    }

    /// Clears a pending undo when a document result is unknowable (for
    /// example, a host HRESULT after SetText). The TSF side abandons the
    /// projection at the same terminal boundary, so the record cannot be
    /// replayed against a document whose text is no longer trusted.
    pub(crate) fn abort_undo_commit(&mut self) -> bool {
        if !self.undo_pending {
            return false;
        }
        self.reset();
        // The host may have applied an edit but failed before the frontend
        // could establish its final text. Neither the pre-undo nor the
        // post-commit carry context is trustworthy now; clear all bounded
        // personal-context state before disarming the record.
        self.clear_personal_context();
        true
    }

    pub(crate) fn disarm_commit_undo(&mut self) {
        self.undo_pending = false;
        self.undo_armed = false;
        self.clear_undo_record();
    }

    fn clear_undo_record(&mut self) {
        self.undo_record.reading.clear();
        self.undo_record.raw_input.clear();
        self.undo_record.shifted_ascii = false;
        self.undo_record.surface.clear();
        self.undo_record.previous_right_id = 0;
        self.undo_record.previous_had_carry = false;
        self.undo_record.post_commit_right_id = 0;
        self.undo_record.post_commit_had_carry = false;
    }

    pub(crate) fn clear_personal_context(&mut self) {
        self.reset_carryover();
        self.commit_cache.fill(CommitCacheEntry::default());
        self.commit_cache_len = 0;
        self.commit_cache_next = 0;
        self.disarm_commit_undo();
    }

    /// Moves only the focused segment's right boundary. Every other boundary
    /// remains byte-for-byte pinned, so distant segmentation cannot change.
    ///
    /// A focused segment that is already the *last* one has no existing
    /// boundary between it and a right neighbor to slide, so it is handled
    /// separately: shrinking instead carves a brand-new boundary out of the
    /// segment's own text (see [`Session::split_trailing_segment`]), which is
    /// how a single mis-guessed bunsetsu becomes splittable at all, and how
    /// the trailing segment of a longer conversion can spawn a new one after
    /// it. Growing still refuses there regardless of how many segments
    /// already exist: `segment_ends`'s last entry is always
    /// `self.preedit.len()` (the whole-reading invariant), so there is never
    /// anything past the last segment for it to absorb. This refusal is
    /// unaffected by a boundary a previous split created elsewhere, because
    /// it is decided purely from whether `index` is still the last segment,
    /// re-read fresh from the live segment count on every call.
    pub(crate) fn resize_focused_segment(&mut self, grow: bool) -> bool {
        let index = self.focused_segment();
        if index + 1 >= self.segment_count() {
            return !grow && self.split_trailing_segment(index);
        }
        let Some(range) = self.segment_range(index) else {
            return false;
        };
        let Some(next) = self.segment_range(index + 1) else {
            return false;
        };
        let new_end = if grow {
            let mut characters = self.preedit.as_str()[next.clone()].char_indices();
            let Some((_, first)) = characters.next() else {
                return false;
            };
            let candidate = next.start + first.len_utf8();
            (candidate < next.end).then_some(candidate)
        } else {
            let mut characters = self.preedit.as_str()[range.clone()].char_indices();
            let Some((last, _)) = characters.next_back() else {
                return false;
            };
            // `char_indices` is relative to this segment slice, not to the
            // whole preedit. A non-zero final offset means at least two
            // characters remain and the boundary can move left safely.
            (last > 0).then_some(range.start + last)
        };
        let Some(new_end) = new_end.and_then(|end| u16::try_from(end).ok()) else {
            return false;
        };
        let Some(boundary) = self.segment_ends.get_mut(index) else {
            return false;
        };
        *boundary = new_end;
        self.segment_selections[index] = 0;
        self.segment_selections[index + 1] = 0;
        self.segment_transforms[index] = SegmentTransform::None;
        self.segment_transforms[index + 1] = SegmentTransform::None;
        self.selected_candidate = 0;
        true
    }

    /// Carves a new boundary out of the focused *trailing* segment: its end
    /// moves one character to the left, and a new segment is appended to
    /// cover exactly the freed remainder. Focus itself is left pointing at
    /// `index` (the now-shorter segment), matching the boundary-slide branch
    /// above, which never moves focus either -- from the user's perspective
    /// this is "make the current segment one character shorter", not "jump
    /// to the new one".
    ///
    /// Refuses (returns `false`), without mutating anything, when:
    /// - `index` is not a real segment (`segment_range` fails), which also
    ///   makes this safe to call on an empty conversion;
    /// - the segment is already a single character, since shrinking it
    ///   further would require producing an empty segment, which the
    ///   `start < end` contract `segment_range` and the rest of this module
    ///   rely on forbids; or
    /// - `segment_ends` is already holding [`MAX_SEGMENTS`] entries. These
    ///   are fixed-capacity, stack-resident collections with no growth path
    ///   past that bound, so a session already at the cap has no slot left
    ///   for the new segment; refusing here is the only option that is
    ///   neither a panic nor a silent truncation of the conversion.
    fn split_trailing_segment(&mut self, index: usize) -> bool {
        let Some(range) = self.segment_range(index) else {
            return false;
        };
        let mut characters = self.preedit.as_str()[range.clone()].char_indices();
        let Some((last, _)) = characters.next_back() else {
            return false;
        };
        if last == 0 {
            return false;
        }
        // `last` is a byte offset relative to this segment's own slice (see
        // the identical note on the boundary-slide branch above), so the
        // real preedit-relative boundary is `range.start + last`.
        let Some(new_end) = u16::try_from(range.start + last).ok() else {
            return false;
        };
        let Some(&old_end) = self.segment_ends.get(index) else {
            return false;
        };
        // The new segment's end is the old segment's end -- appending it
        // keeps the "last end equals preedit length" invariant intact
        // without needing to touch any other entry, since `index` was
        // already the last one (that is this function's only caller's
        // precondition). `push` is the one point that can fail: once
        // `segment_ends` already holds MAX_SEGMENTS entries there is no
        // capacity for a new segment, and this must refuse rather than
        // panic or drop the conversion's tail silently.
        if self.segment_ends.push(old_end).is_err() {
            return false;
        }
        let Some(boundary) = self.segment_ends.get_mut(index) else {
            // Unreachable: `index` resolved to a live entry a few lines
            // above and this function never removes entries in between, but
            // this stays a refusal rather than an assumption.
            return false;
        };
        *boundary = new_end;
        let new_index = index + 1;
        // Both halves of the split have unresolved candidates for their new
        // (shorter) reading text, so neither may keep the old selection or
        // transform: `render_converted_segments`/`commit_converted_segments`
        // in `dispatch.rs` re-derive each segment's reading from
        // `segment_range` fresh on every call, so resetting these to a sane
        // default here is enough for the very next render to show correct
        // candidates for both segments.
        self.segment_selections[index] = 0;
        self.segment_selections[new_index] = 0;
        self.segment_transforms[index] = SegmentTransform::None;
        self.segment_transforms[new_index] = SegmentTransform::None;
        self.segment_transform_cycles[index] = 0;
        self.segment_transform_cycles[new_index] = 0;
        self.selected_candidate = 0;
        true
    }
}

pub(crate) fn text_hash(text: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    text.as_bytes().iter().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    })
}

fn is_sentence_boundary(surface: &str) -> bool {
    let trimmed = surface.trim_end();
    trimmed.ends_with(['。', '！', '？'])
        || trimmed.ends_with("です")
        || trimmed.ends_with("ます")
        || trimmed.ends_with("でした")
        || trimmed.ends_with("ました")
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

/// A fixed-capacity map from [`SessionId`] to [`Session`], allocation-free
/// once built.
///
/// Backed by exactly [`MAX_SESSIONS`] slots rather than a `HashMap` — this
/// crate's whole point is a bounded footprint per connection (DESIGN 5.7,
/// DESIGN 10's 15 MB budget), and a `HashMap` is exactly the
/// unbounded-growth structure that contradicts it. The slots are not
/// `sakura_proto::FixedVec<(SessionId, Session), N>` either: `FixedVec`
/// requires `T: Copy + Default` so that an empty slot needs no drop
/// handling, and `Session` holds a `FixedStr` (`Clone`, not `Copy`, so that
/// cloning stays an explicit, visible cost — see [`Session`]'s docs). A
/// slice of `Option` needs no such bound and costs nothing extra: a `None`
/// slot is exactly as cheap as an unused `FixedVec` slot would have been.
///
/// The slots live in a boxed slice rather than an inline
/// `[Option<(SessionId, Session)>; MAX_SESSIONS]`. Sixty-four sessions each
/// holding a preedit-sized [`FixedStr`] is ~107 KB, and inline that lands
/// wherever the table is built — including on the 128 KB pipe-worker stack
/// `crate::server` deliberately reserves, which it overflowed. Bounded is
/// the promise; *inline* was never part of it. One allocation happens here,
/// at connection setup, and never again: `SendKey` still touches nothing but
/// already-owned memory, which is what `tests/zero_alloc_dispatch.rs`
/// measures.
#[derive(Debug)]
pub struct SessionTable {
    slots: Box<[Option<(SessionId, Session)>]>,
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
        // Grown through a `Vec` rather than boxing an array literal:
        // `Box::new([...; MAX_SESSIONS])` builds the whole ~107 KB array in
        // the caller's frame first and only then copies it to the heap, and
        // that temporary is what has to not exist here (see the type's
        // docs). `resize_with` writes each slot straight into the heap
        // buffer `with_capacity` already reserved.
        let mut slots = Vec::with_capacity(MAX_SESSIONS);
        slots.resize_with(MAX_SESSIONS, || None);
        SessionTable {
            slots: slots.into_boxed_slice(),
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
        for slot in self.slots.iter_mut() {
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
        for slot in self.slots.iter_mut() {
            *slot = None;
        }
        self.len = 0;
    }

    /// Applies settings that are safe to change while a host context remains
    /// alive. The user's current mode and composition are deliberately left
    /// untouched; only the policies that are resolved from the process profile
    /// are refreshed for the next request.
    pub(crate) fn apply_runtime_preferences(
        &mut self,
        preferences: Preferences,
        profiles: &[AppProfile],
    ) {
        for slot in self.slots.iter_mut().flatten() {
            let resolved =
                resolve_context_preferences(preferences, profiles, slot.1.process_name());
            slot.1.normalizer = resolved.normalizer;
            slot.1.space_width = resolved.space_width;
            slot.1.shift_space_behavior = resolved.shift_space_behavior;
            slot.1.input_method = resolved.input_method;
            // Do not alter an active composition's lattice policy halfway
            // through conversion. The new value is picked up by the next
            // idle context; this keeps the candidate snapshot coherent.
            if slot.1.preedit.is_empty() && !slot.1.converting {
                slot.1.conversion_method = resolved.conversion_method;
            }
            slot.1.prediction_enabled = resolved.prediction_enabled;
            slot.1.suggest_accept = resolved.suggest_accept;
            slot.1.association_enabled = resolved.association_enabled;
        }
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
    fn runtime_preferences_update_live_policy_without_resetting_mode_or_text() {
        let mut table = SessionTable::new();
        let id = table.create("notepad.exe").expect("session");
        let session = table.get_mut(id).expect("live session");
        session.mode = Mode::Katakana;
        session.preedit.push_str("かな").expect("preedit");

        let mut normalizer = Normalizer::default();
        normalizer.width.alnum = sakura_core::Width::Full;
        let preferences = Preferences {
            normalizer,
            prediction_enabled: false,
            association_enabled: false,
            suggest_accept: SuggestAccept::ShiftEnter,
            ..Preferences::default()
        };
        table.apply_runtime_preferences(preferences, &[]);

        let session = table.get(id).expect("live session");
        assert_eq!(session.mode, Mode::Katakana);
        assert_eq!(session.preedit.as_str(), "かな");
        assert_eq!(session.normalizer.width.alnum, sakura_core::Width::Full);
        assert!(!session.prediction_enabled);
        assert_eq!(session.suggest_accept, SuggestAccept::ShiftEnter);
        assert!(!session.association_enabled);
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
    fn segment_resize_changes_only_the_focused_boundary_and_is_reversible() {
        let mut session = Session::new("editor.exe");
        session.preedit.push_str("あいうえおかきく").expect("fits");
        let segments = [6u16, 12, 18, 24].map(|reading_end| ConversionSegment {
            reading_end,
            ..ConversionSegment::default()
        });
        assert!(session.set_segments(&segments));
        session.converting = true;
        session.focus_next_segment();
        assert_eq!(session.focused_segment(), 1);

        assert!(session.resize_focused_segment(true));
        assert_eq!(session.segment_range(0), Some(0..6));
        assert_eq!(session.segment_range(1), Some(6..15));
        assert_eq!(session.segment_range(2), Some(15..18));
        assert_eq!(session.segment_range(3), Some(18..24));

        assert!(session.resize_focused_segment(false));
        assert_eq!(session.segment_range(0), Some(0..6));
        assert_eq!(session.segment_range(1), Some(6..12));
        assert_eq!(session.segment_range(2), Some(12..18));
        assert_eq!(session.segment_range(3), Some(18..24));
    }

    #[test]
    fn resize_focused_segment_splits_a_single_bunsetsu_conversion_on_shrink() {
        // The common case this fixes: the engine guessed the whole reading
        // as one segment, and Shift+Left must still be able to re-cut it.
        let mut session = Session::new("editor.exe");
        session.preedit.push_str("あいう").expect("fits");
        let segments = [ConversionSegment {
            reading_end: 9,
            ..ConversionSegment::default()
        }];
        assert!(session.set_segments(&segments));
        session.converting = true;
        assert_eq!(session.segment_count(), 1);
        assert_eq!(session.focused_segment(), 0);

        // Growing the only segment has nothing past the conversion's end to
        // absorb, so it must keep refusing exactly as it did before this
        // segment could ever split.
        assert!(!session.resize_focused_segment(true));
        assert_eq!(session.segment_count(), 1);

        assert!(session.resize_focused_segment(false));
        assert_eq!(session.segment_count(), 2);
        assert_eq!(session.segment_range(0), Some(0..6));
        assert_eq!(session.segment_range(1), Some(6..9));
        // Both boundaries land on character boundaries -- "う" (3 bytes)
        // stays intact in the new trailing segment, never split in half.
        assert!(session.preedit.as_str().is_char_boundary(6));
        assert!(session.preedit.as_str().is_char_boundary(9));
        assert_eq!(&session.preedit.as_str()[0..6], "あい");
        assert_eq!(&session.preedit.as_str()[6..9], "う");
        // Focus stays on the segment that was resized, matching the
        // boundary-slide branch, which never moves focus either.
        assert_eq!(session.focused_segment(), 0);
        assert_eq!(session.segment_selection(0), 0);
        assert_eq!(session.segment_selection(1), 0);
        assert_eq!(session.segment_transform(0), (SegmentTransform::None, 0));
        assert_eq!(session.segment_transform(1), (SegmentTransform::None, 0));
    }

    #[test]
    fn resize_focused_segment_refuses_to_shrink_a_one_character_segment() {
        let mut session = Session::new("editor.exe");
        session.preedit.push_str("あ").expect("fits");
        let segments = [ConversionSegment {
            reading_end: 3,
            ..ConversionSegment::default()
        }];
        assert!(session.set_segments(&segments));
        session.converting = true;

        assert!(!session.resize_focused_segment(false));
        assert_eq!(session.segment_count(), 1);
        assert_eq!(session.segment_range(0), Some(0..3));
    }

    #[test]
    fn resize_focused_segment_splits_the_trailing_segment_of_a_multi_segment_conversion() {
        let mut session = Session::new("editor.exe");
        session.preedit.push_str("あいうえおかきく").expect("fits");
        let segments = [6u16, 12, 18, 24].map(|reading_end| ConversionSegment {
            reading_end,
            ..ConversionSegment::default()
        });
        assert!(session.set_segments(&segments));
        session.converting = true;
        session.focus_next_segment();
        session.focus_next_segment();
        session.focus_next_segment();
        assert_eq!(session.focused_segment(), 3);

        // Growing the trailing segment still refuses: there is nothing past
        // the conversion's end for it to absorb.
        assert!(!session.resize_focused_segment(true));
        assert_eq!(session.segment_count(), 4);

        assert!(session.resize_focused_segment(false));
        assert_eq!(session.segment_count(), 5);
        assert_eq!(session.segment_range(0), Some(0..6));
        assert_eq!(session.segment_range(1), Some(6..12));
        assert_eq!(session.segment_range(2), Some(12..18));
        assert_eq!(session.segment_range(3), Some(18..21));
        assert_eq!(session.segment_range(4), Some(21..24));
        assert!(session.preedit.as_str().is_char_boundary(21));
        assert_eq!(&session.preedit.as_str()[18..21], "き");
        assert_eq!(&session.preedit.as_str()[21..24], "く");
        // Earlier boundaries (0..18) are untouched, matching the
        // boundary-slide branch's "every other boundary stays pinned"
        // guarantee.
        assert_eq!(session.focused_segment(), 3);
        assert_eq!(session.segment_selection(4), 0);
        assert_eq!(session.segment_transform(4), (SegmentTransform::None, 0));
    }

    #[test]
    fn resize_focused_segment_refuses_to_split_once_segment_count_is_at_capacity() {
        // MAX_SEGMENTS - 1 one-character segments, plus a trailing
        // two-character segment: exactly MAX_SEGMENTS segments already, with
        // the trailing one otherwise splittable on every ground except room.
        let mut session = Session::new("editor.exe");
        let mut reading = String::new();
        let mut ends = Vec::new();
        for _ in 0..MAX_SEGMENTS - 1 {
            reading.push('x');
            ends.push(u16::try_from(reading.len()).expect("fits"));
        }
        reading.push_str("yz");
        ends.push(u16::try_from(reading.len()).expect("fits"));
        session.preedit.push_str(&reading).expect("fits");
        let segments: Vec<ConversionSegment> = ends
            .into_iter()
            .map(|reading_end| ConversionSegment {
                reading_end,
                ..ConversionSegment::default()
            })
            .collect();
        assert!(session.set_segments(&segments));
        session.converting = true;
        assert_eq!(session.segment_count(), MAX_SEGMENTS);
        for _ in 0..MAX_SEGMENTS - 1 {
            session.focus_next_segment();
        }
        assert_eq!(session.focused_segment(), MAX_SEGMENTS - 1);

        assert!(!session.resize_focused_segment(false));
        assert_eq!(session.segment_count(), MAX_SEGMENTS);
    }

    #[test]
    fn commit_undo_cache_restores_reading_and_rolls_back_context() {
        let mut session = Session::new("editor.exe");
        session.preedit.push_str("かな").expect("fits");
        session.raw_input.push_str("kana").expect("fits");
        session.record_current_commit("加奈", 42, 0, 1);
        let cached = session.cached_surface_fingerprint("かな");
        assert_eq!(cached, Some((text_hash("加奈"), 6)));
        assert_eq!(session.carry_right_id(), 42);
        session.reset();

        assert!(session.undo_commit().is_some());
        assert_eq!(session.preedit.as_str(), "かな");
        assert_eq!(session.raw_input.as_str(), "kana");
        assert_eq!(session.carry_right_id(), 0);
        assert_eq!(
            session.cached_surface_fingerprint("かな"),
            cached,
            "the recency entry remains live until the host acknowledges deletion"
        );
        assert!(session.reject_undo_commit());
        assert_eq!(session.carry_right_id(), 42);
        assert!(session.undo_commit().is_some());
        assert!(session.acknowledge_undo_commit());
        assert_eq!(session.cached_surface_fingerprint("かな"), None);
        assert_eq!(session.undo_commit(), None, "undo depth is exactly one");
    }

    #[test]
    fn commit_undo_rejection_and_ack_keep_engine_and_document_terminal_states_aligned() {
        let mut session = Session::new("editor.exe");
        session.preedit.push_str("reading").expect("fits");
        session.raw_input.push_str("raw").expect("fits");
        session.record_current_commit("committed", 7, 0, 1);
        session.reset();

        // A moved caret/text mismatch is a host rejection: the simulated
        // document is untouched, while the engine returns to its exact
        // post-commit idle state and keeps the bounded undo record.
        let mut document = String::from("prefixother");
        let expected = session.undo_commit().expect("undo is pending");
        assert_eq!(expected.as_str(), "committed");
        assert!(session.undo_pending());
        if !document.ends_with(expected.as_str()) {
            assert!(session.reject_undo_commit());
        }
        assert_eq!(document, "prefixother");
        assert!(!session.undo_pending());
        assert!(!session.is_composing());
        assert_eq!(session.carry_right_id(), 7);
        assert!(session.undo_commit().is_some(), "rejection preserves undo");

        // Exact verification succeeds: only then does the simulated host
        // delete and the engine consume its record/cache entry.
        let expected = session.undo_record.surface.as_str().to_owned();
        document.push_str("committed");
        assert!(document.ends_with(&expected));
        document.truncate(document.len() - expected.len());
        assert!(session.acknowledge_undo_commit());
        assert_eq!(document, "prefixother");
        assert!(!session.undo_pending());
        assert!(session.is_composing());
        assert_eq!(session.cached_surface_fingerprint("reading"), None);
        assert!(session.undo_commit().is_none(), "ack consumes undo");
    }

    #[test]
    fn commit_undo_wrapped_commit_history_uses_only_the_live_ring_window() {
        let mut session = Session::new("editor.exe");
        for _ in 0..COMMIT_CACHE_CAPACITY {
            session.preedit.push_str("いった").expect("fits");
            session.record_current_commit("言った", 1, 0, 1);
            session.reset();
        }
        for _ in 0..4 {
            session.preedit.push_str("かんすう").expect("fits");
            session.record_current_commit("関数", 2, 1, 1);
            session.reset();
        }

        assert_eq!(
            session.domain_it_ratio_per_mille(),
            u16::try_from(4_000 / COMMIT_CACHE_CAPACITY).expect("ratio")
        );
        assert!(session.undo_commit().is_some());
        assert_eq!(
            session.domain_it_ratio_per_mille(),
            u16::try_from(4_000 / COMMIT_CACHE_CAPACITY).expect("ratio"),
            "the recency entry remains until the host reports Applied"
        );
        assert!(session.acknowledge_undo_commit());
        assert_eq!(
            session.domain_it_ratio_per_mille(),
            u16::try_from(3_000 / (COMMIT_CACHE_CAPACITY - 1)).expect("ratio")
        );
    }

    #[test]
    fn commit_undo_unknown_clears_untrusted_carry_and_personal_context() {
        let mut session = Session::new("editor.exe");
        session.preedit.push_str("かな").expect("fits");
        session.record_current_commit("加奈", 77, 1, 1);
        session.reset();
        assert_eq!(session.carry_right_id(), 77);
        assert!(session.has_carry);
        assert!(session.cached_surface_fingerprint("かな").is_some());

        assert!(session.undo_commit().is_some());
        assert!(session.undo_pending());
        assert!(session.abort_undo_commit());

        assert!(!session.undo_pending());
        assert_eq!(session.carry_right_id(), 0);
        assert!(!session.has_carry);
        assert_eq!(session.domain_it_ratio_per_mille(), 0);
        assert_eq!(session.cached_surface_fingerprint("かな"), None);
        assert!(session.undo_commit().is_none());
    }

    #[test]
    fn sentence_boundary_and_sensitive_scope_gate_recent_context() {
        let mut session = Session::new("editor.exe");
        session.preedit.push_str("いしゃに").expect("fits");
        session.record_current_commit("医者に", 42, 0, 2);
        assert_eq!(session.carry_right_id(), 42);
        session.reset();

        session.preedit.push_str("おわり").expect("fits");
        session.record_current_commit("終わり。", 9, 0, 1);
        assert_eq!(session.carry_right_id(), 0);
        session.reset();

        session.scope = InputScope::Password;
        session.preedit.push_str("ひみつ").expect("fits");
        session.record_current_commit("秘密", 7, 1, 1);
        assert_eq!(session.carry_right_id(), 0);
        assert_eq!(session.cached_surface_fingerprint("ひみつ"), None);
        session.reset();
        assert_eq!(session.undo_commit(), None);
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
