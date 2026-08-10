//! `OutputBuf`: a fixed-capacity, allocation-free builder for
//! `Response::Output` frames.
//!
//! The engine's hot path (kana input, conversion, prediction hand-off)
//! must not allocate (DESIGN.md §5.7, §10). Building a `types::Output`
//! the normal way allocates a `Vec<Segment>` and one `String` per segment
//! plus one for the commit text; `OutputBuf` instead accumulates preedit
//! and commit text into [`FixedStr`] buffers and segment spans into a
//! [`FixedVec`], then encodes straight to bytes with [`OutputBuf::encode_frame`]
//! — no intermediate owned `Output` is built on that path at all.
//! [`OutputBuf::to_output`] is provided for tests and the DLL side, where
//! an owned `Output` is convenient and allocation is not a concern.

use core::ptr;

use crate::fixed::{FixedStr, FixedVec, Overflow};
use crate::message::RES_OUTPUT;
use crate::types::{
    Candidate, CandidateDetail, CandidateKind, CandidateList, CandidatePresentation, Mode, Output,
    Preedit, Segment, UnderlineKind,
};
use crate::wire::{Error, Sink, SliceSink};
use crate::{
    RequestId, CANDIDATE_PAGE_SIZE, FRAME_HEADER_LEN, MAX_CANDIDATES,
    MAX_CANDIDATE_DETAIL_DEFINITION_BYTES, MAX_CANDIDATE_DETAIL_READING_BYTES,
    MAX_CANDIDATE_DETAIL_RELATIONS, MAX_CANDIDATE_DETAIL_RELATION_BYTES,
    MAX_CANDIDATE_DETAIL_RELATION_TEXT_BYTES, MAX_CANDIDATE_TEXT_BYTES, MAX_COMMIT_BYTES,
    MAX_PAYLOAD, MAX_PREEDIT_BYTES, MAX_SEGMENTS, PROTOCOL_VERSION,
};

/// One segment's span within `OutputBuf`'s flat preedit text buffer.
///
/// `start`/`len` are **byte** offsets/lengths into
/// [`OutputBuf::preedit_text`] — segments are always pushed as whole
/// `&str` values, so a span's boundaries are always UTF-8 char
/// boundaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SegSpan {
    pub start: u32,
    pub len: u32,
    pub underline: UnderlineKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct CandidateSpan {
    text_start: u32,
    text_len: u32,
    annotation_start: u32,
    annotation_len: u32,
    /// A private engine-to-UI-board capability, not part of the regular
    /// `Output` wire shape. Zero means this row is not a learned history row.
    history_reading_start: u32,
    history_reading_len: u32,
    history_surface_start: u32,
    history_surface_len: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct DetailSpan {
    start: u32,
    len: u32,
}

/// Borrowed input for [`OutputBuf::set_candidate_detail`].
///
/// The struct keeps the mutation API small while making the bounded preview
/// and its completeness flag impossible to confuse at the call site.
#[derive(Debug, Clone, Copy)]
pub struct CandidateDetailInput<'a> {
    pub reading: &'a str,
    pub definition: &'a str,
    pub definition_truncated: bool,
    pub aliases: &'a [&'a str],
    pub related: &'a [&'a str],
    pub similar: &'a [&'a str],
    pub antonyms: &'a [&'a str],
}

/// Allocation-free borrowed view of selected-candidate detail.
#[derive(Debug, Clone, Copy)]
pub struct CandidateDetailRef<'a> {
    pub reading: &'a str,
    pub definition: &'a str,
    pub definition_truncated: bool,
    relation_text: &'a str,
    aliases: &'a [DetailSpan],
    related: &'a [DetailSpan],
    similar: &'a [DetailSpan],
    antonyms: &'a [DetailSpan],
}

impl<'a> CandidateDetailRef<'a> {
    pub fn aliases(&self) -> CandidateDetailTerms<'a> {
        CandidateDetailTerms::new(self.relation_text, self.aliases)
    }

    pub fn related(&self) -> CandidateDetailTerms<'a> {
        CandidateDetailTerms::new(self.relation_text, self.related)
    }

    pub fn similar(&self) -> CandidateDetailTerms<'a> {
        CandidateDetailTerms::new(self.relation_text, self.similar)
    }

    pub fn antonyms(&self) -> CandidateDetailTerms<'a> {
        CandidateDetailTerms::new(self.relation_text, self.antonyms)
    }
}

/// Cloneable, exact-size iterator over one bounded relation group.
#[derive(Debug, Clone)]
pub struct CandidateDetailTerms<'a> {
    text: &'a str,
    spans: core::slice::Iter<'a, DetailSpan>,
}

impl<'a> CandidateDetailTerms<'a> {
    fn new(text: &'a str, spans: &'a [DetailSpan]) -> Self {
        Self {
            text,
            spans: spans.iter(),
        }
    }
}

impl<'a> Iterator for CandidateDetailTerms<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let span = *self.spans.next()?;
        let start = span.start as usize;
        Some(
            self.text
                .get(start..start + span.len as usize)
                .unwrap_or(""),
        )
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.spans.size_hint()
    }
}

impl ExactSizeIterator for CandidateDetailTerms<'_> {}
impl core::iter::FusedIterator for CandidateDetailTerms<'_> {}

/// A fixed-capacity, allocation-free builder for a `Response::Output`
/// frame.
///
/// All text lives in [`FixedStr`] buffers and all segment spans in a
/// [`FixedVec`]; every field is stack-sized, so [`OutputBuf::new`] and
/// every mutator run without touching the heap.
#[derive(Clone)]
pub struct OutputBuf {
    pub consumed: bool,
    pub beep: bool,
    pub mode: Option<Mode>,
    has_preedit: bool,
    preedit: FixedStr<MAX_PREEDIT_BYTES>,
    segments: FixedVec<SegSpan, MAX_SEGMENTS>,
    cursor: u32,
    has_commit: bool,
    commit: FixedStr<MAX_COMMIT_BYTES>,
    delete_before: FixedStr<MAX_COMMIT_BYTES>,
    has_candidates: bool,
    candidate_kind: CandidateKind,
    candidate_presentation: CandidatePresentation,
    candidate_text: FixedStr<MAX_CANDIDATE_TEXT_BYTES>,
    candidate_annotations: FixedStr<MAX_CANDIDATE_TEXT_BYTES>,
    /// Exact, unnormalised learning keys for history rows. These never leave
    /// the engine as part of `Response::Output`; the shared UI board uses them
    /// to authorize a revision-stamped renderer delete request.
    candidate_history_readings: FixedStr<MAX_CANDIDATE_TEXT_BYTES>,
    candidate_history_surfaces: FixedStr<MAX_CANDIDATE_TEXT_BYTES>,
    candidates: FixedVec<CandidateSpan, MAX_CANDIDATES>,
    selected_candidate: u16,
    candidate_page_size: u16,
    has_candidate_detail: bool,
    detail_reading: FixedStr<MAX_CANDIDATE_DETAIL_READING_BYTES>,
    detail_definition: FixedStr<MAX_CANDIDATE_DETAIL_DEFINITION_BYTES>,
    detail_definition_truncated: bool,
    detail_relations: FixedStr<MAX_CANDIDATE_DETAIL_RELATION_TEXT_BYTES>,
    detail_aliases: FixedVec<DetailSpan, MAX_CANDIDATE_DETAIL_RELATIONS>,
    detail_related: FixedVec<DetailSpan, MAX_CANDIDATE_DETAIL_RELATIONS>,
    detail_similar: FixedVec<DetailSpan, MAX_CANDIDATE_DETAIL_RELATIONS>,
    detail_antonyms: FixedVec<DetailSpan, MAX_CANDIDATE_DETAIL_RELATIONS>,
}

impl OutputBuf {
    /// Creates an empty `OutputBuf`. Does not allocate.
    pub fn new() -> Self {
        OutputBuf {
            consumed: false,
            beep: false,
            mode: None,
            has_preedit: false,
            preedit: FixedStr::new(),
            segments: FixedVec::new(),
            cursor: 0,
            has_commit: false,
            commit: FixedStr::new(),
            delete_before: FixedStr::new(),
            has_candidates: false,
            candidate_kind: CandidateKind::Conversion,
            candidate_presentation: CandidatePresentation::Compact,
            candidate_text: FixedStr::new(),
            candidate_annotations: FixedStr::new(),
            candidate_history_readings: FixedStr::new(),
            candidate_history_surfaces: FixedStr::new(),
            candidates: FixedVec::new(),
            selected_candidate: 0,
            candidate_page_size: CANDIDATE_PAGE_SIZE as u16,
            has_candidate_detail: false,
            detail_reading: FixedStr::new(),
            detail_definition: FixedStr::new(),
            detail_definition_truncated: false,
            detail_relations: FixedStr::new(),
            detail_aliases: FixedVec::new(),
            detail_related: FixedVec::new(),
            detail_similar: FixedVec::new(),
            detail_antonyms: FixedVec::new(),
        }
    }

    /// Allocates and initializes an empty `OutputBuf` directly in its final
    /// heap storage.
    ///
    /// This is for thread setup, where building a whole [`OutputBuf`] first
    /// would put its large fixed buffers on a deliberately small worker stack
    /// before `Box::new` could move it. [`OutputBuf::new`] remains the
    /// allocation-free value constructor, and every mutation after this one
    /// setup allocation remains allocation-free.
    pub fn new_boxed() -> Box<Self> {
        let mut boxed = Box::<Self>::new_uninit();
        let output = boxed.as_mut_ptr();

        // SAFETY: `output` points to uniquely owned, uninitialized storage
        // for exactly one `OutputBuf`. Every field is written exactly once
        // with the same value as `new()` and none of these constructors can
        // panic. No reference to a partially initialized value escapes.
        unsafe {
            ptr::addr_of_mut!((*output).consumed).write(false);
            ptr::addr_of_mut!((*output).beep).write(false);
            ptr::addr_of_mut!((*output).mode).write(None);
            ptr::addr_of_mut!((*output).has_preedit).write(false);
            ptr::addr_of_mut!((*output).preedit).write(FixedStr::new());
            ptr::addr_of_mut!((*output).segments).write(FixedVec::new());
            ptr::addr_of_mut!((*output).cursor).write(0);
            ptr::addr_of_mut!((*output).has_commit).write(false);
            ptr::addr_of_mut!((*output).commit).write(FixedStr::new());
            ptr::addr_of_mut!((*output).delete_before).write(FixedStr::new());
            ptr::addr_of_mut!((*output).has_candidates).write(false);
            ptr::addr_of_mut!((*output).candidate_kind).write(CandidateKind::Conversion);
            ptr::addr_of_mut!((*output).candidate_presentation)
                .write(CandidatePresentation::Compact);
            ptr::addr_of_mut!((*output).candidate_text).write(FixedStr::new());
            ptr::addr_of_mut!((*output).candidate_annotations).write(FixedStr::new());
            ptr::addr_of_mut!((*output).candidate_history_readings).write(FixedStr::new());
            ptr::addr_of_mut!((*output).candidate_history_surfaces).write(FixedStr::new());
            ptr::addr_of_mut!((*output).candidates).write(FixedVec::new());
            ptr::addr_of_mut!((*output).selected_candidate).write(0);
            ptr::addr_of_mut!((*output).candidate_page_size).write(CANDIDATE_PAGE_SIZE as u16);
            ptr::addr_of_mut!((*output).has_candidate_detail).write(false);
            ptr::addr_of_mut!((*output).detail_reading).write(FixedStr::new());
            ptr::addr_of_mut!((*output).detail_definition).write(FixedStr::new());
            ptr::addr_of_mut!((*output).detail_definition_truncated).write(false);
            ptr::addr_of_mut!((*output).detail_relations).write(FixedStr::new());
            ptr::addr_of_mut!((*output).detail_aliases).write(FixedVec::new());
            ptr::addr_of_mut!((*output).detail_related).write(FixedVec::new());
            ptr::addr_of_mut!((*output).detail_similar).write(FixedVec::new());
            ptr::addr_of_mut!((*output).detail_antonyms).write(FixedVec::new());
        }

        // SAFETY: the field-wise initialization above completed every field
        // of the allocated `OutputBuf` exactly once.
        unsafe { boxed.assume_init() }
    }

    /// Resets every field to its default, ready for reuse. Does not
    /// allocate (and releases no capacity — the underlying buffers keep
    /// their fixed size).
    pub fn clear(&mut self) {
        self.consumed = false;
        self.beep = false;
        self.mode = None;
        self.has_preedit = false;
        self.preedit.clear();
        self.segments.clear();
        self.cursor = 0;
        self.has_commit = false;
        self.commit.clear();
        self.delete_before.clear();
        self.has_candidates = false;
        self.candidate_kind = CandidateKind::Conversion;
        self.candidate_presentation = CandidatePresentation::Compact;
        self.candidate_text.clear();
        self.candidate_annotations.clear();
        self.candidate_history_readings.clear();
        self.candidate_history_surfaces.clear();
        self.candidates.clear();
        self.selected_candidate = 0;
        self.candidate_page_size = CANDIDATE_PAGE_SIZE as u16;
        self.clear_candidate_detail();
    }

    /// Starts (or restarts) a preedit composition: marks a preedit as
    /// present and clears any previously accumulated segment text/spans
    /// and cursor.
    pub fn begin_preedit(&mut self) {
        self.has_preedit = true;
        self.preedit.clear();
        self.segments.clear();
        self.cursor = 0;
    }

    /// Appends one segment of preedit text.
    ///
    /// Atomic on overflow: if `text` would not fit in the remaining
    /// preedit capacity, or the segment table is already full, neither
    /// the preedit text nor the segment table is changed.
    pub fn push_segment(&mut self, text: &str, underline: UnderlineKind) -> Result<(), Overflow> {
        if self.segments.len() >= self.segments.capacity() {
            return Err(Overflow);
        }
        let start = self.preedit.len();
        let new_len = match start.checked_add(text.len()) {
            Some(n) if n <= self.preedit.capacity() => n,
            _ => return Err(Overflow),
        };
        // Capacity for both buffers was checked above, so these cannot
        // fail; `?` still routes any surprise failure through cleanly
        // instead of unwrapping.
        self.preedit.push_str(text)?;
        let span = SegSpan {
            start: start as u32,
            len: (new_len - start) as u32,
            underline,
        };
        self.segments.push(span)?;
        Ok(())
    }

    /// Sets the preedit cursor, a **character** offset into the
    /// concatenation of all segment texts (matching
    /// [`crate::types::Preedit::cursor`]).
    pub fn set_cursor(&mut self, chars: u32) {
        self.cursor = chars;
    }

    /// Sets the commit text, replacing any previous value.
    ///
    /// Atomic on overflow: if `text` does not fit, the previous commit
    /// text (if any) is left unchanged.
    pub fn set_commit(&mut self, text: &str) -> Result<(), Overflow> {
        if text.len() > self.commit.capacity() {
            return Err(Overflow);
        }
        self.commit.clear();
        self.commit.push_str(text)?;
        self.has_commit = true;
        Ok(())
    }

    /// Appends to the commit text, preserving a commit produced earlier in
    /// the same key event. This is needed when a mode-switching IME commits
    /// an existing preedit and then emits a temporary direct-input character.
    pub fn append_commit(&mut self, text: &str) -> Result<(), Overflow> {
        let Some(new_len) = self.commit.len().checked_add(text.len()) else {
            return Err(Overflow);
        };
        if new_len > self.commit.capacity() {
            return Err(Overflow);
        }
        self.commit.push_str(text)?;
        self.has_commit = true;
        Ok(())
    }

    /// Requests exact-text verification and deletion immediately before the
    /// host caret. Empty disables commit undo.
    pub fn set_delete_before(&mut self, text: &str) -> Result<(), Overflow> {
        if text.len() > self.delete_before.capacity() {
            return Err(Overflow);
        }
        self.delete_before.clear();
        self.delete_before.push_str(text)
    }

    pub fn delete_before(&self) -> &str {
        self.delete_before.as_str()
    }

    /// Returns the UTF-16 width for the legacy input-history diagnostic field.
    pub fn delete_before_utf16(&self) -> u16 {
        u16::try_from(self.delete_before.as_str().encode_utf16().count()).unwrap_or(u16::MAX)
    }

    /// Returns the accumulated preedit text (the concatenation of every
    /// pushed segment).
    pub fn preedit_text(&self) -> &str {
        self.preedit.as_str()
    }

    /// Returns the commit text, if `set_commit` has been called since the
    /// last `clear`.
    pub fn commit_text(&self) -> Option<&str> {
        if self.has_commit {
            Some(self.commit.as_str())
        } else {
            None
        }
    }

    pub fn begin_candidates(&mut self, selected: u16, page_size: u16) -> Result<(), Overflow> {
        self.begin_candidate_list(
            CandidateKind::Conversion,
            CandidatePresentation::Compact,
            selected,
            page_size,
        )
    }

    /// Starts conversion candidates with the requested presentation. The
    /// complete bounded list remains in the builder; compactness is consumed
    /// by presentation-aware renderer/accessibility clients only.
    pub fn begin_conversion_candidates(
        &mut self,
        presentation: CandidatePresentation,
        selected: u16,
        page_size: u16,
    ) -> Result<(), Overflow> {
        self.begin_candidate_list(CandidateKind::Conversion, presentation, selected, page_size)
    }

    /// Starts an inline prediction list. It shares the fixed candidate buffers
    /// with conversion because the two UI states are mutually exclusive.
    pub fn begin_suggestions(&mut self, selected: u16, page_size: u16) -> Result<(), Overflow> {
        self.begin_candidate_list(
            CandidateKind::Suggestion,
            CandidatePresentation::Expanded,
            selected,
            page_size,
        )
    }

    fn begin_candidate_list(
        &mut self,
        kind: CandidateKind,
        presentation: CandidatePresentation,
        selected: u16,
        page_size: u16,
    ) -> Result<(), Overflow> {
        if page_size == 0 || usize::from(selected) >= MAX_CANDIDATES {
            return Err(Overflow);
        }
        self.has_candidates = true;
        self.candidate_kind = kind;
        self.candidate_presentation = presentation;
        self.candidate_text.clear();
        self.candidate_annotations.clear();
        self.candidate_history_readings.clear();
        self.candidate_history_surfaces.clear();
        self.candidates.clear();
        self.selected_candidate = selected;
        self.candidate_page_size = page_size;
        self.clear_candidate_detail();
        Ok(())
    }

    pub fn push_candidate(&mut self, text: &str, annotation: &str) -> Result<(), Overflow> {
        self.push_candidate_inner(text, annotation, None)
    }

    /// Pushes a rendered prediction row with the exact persisted history key
    /// retained for the engine-owned UI board. This metadata remains private
    /// to the engine process: ordinary output consumers still receive only the
    /// candidate surface and annotation.
    pub fn push_history_candidate(
        &mut self,
        text: &str,
        annotation: &str,
        reading: &str,
        surface: &str,
    ) -> Result<(), Overflow> {
        if reading.is_empty() || surface.is_empty() {
            return Err(Overflow);
        }
        self.push_candidate_inner(text, annotation, Some((reading, surface)))
    }

    fn push_candidate_inner(
        &mut self,
        text: &str,
        annotation: &str,
        history: Option<(&str, &str)>,
    ) -> Result<(), Overflow> {
        if !self.has_candidates || self.candidates.len() >= MAX_CANDIDATES {
            return Err(Overflow);
        }
        let text_start = self.candidate_text.len();
        let annotation_start = self.candidate_annotations.len();
        let reading_start = self.candidate_history_readings.len();
        let surface_start = self.candidate_history_surfaces.len();
        if text_start.saturating_add(text.len()) > self.candidate_text.capacity()
            || annotation_start.saturating_add(annotation.len())
                > self.candidate_annotations.capacity()
            || history.is_some_and(|(reading, _)| {
                reading_start.saturating_add(reading.len())
                    > self.candidate_history_readings.capacity()
            })
            || history.is_some_and(|(_, surface)| {
                surface_start.saturating_add(surface.len())
                    > self.candidate_history_surfaces.capacity()
            })
        {
            return Err(Overflow);
        }
        self.candidate_text.push_str(text)?;
        self.candidate_annotations.push_str(annotation)?;
        if let Some((reading, surface)) = history {
            self.candidate_history_readings.push_str(reading)?;
            self.candidate_history_surfaces.push_str(surface)?;
        }
        self.candidates.push(CandidateSpan {
            text_start: text_start as u32,
            text_len: text.len() as u32,
            annotation_start: annotation_start as u32,
            annotation_len: annotation.len() as u32,
            history_reading_start: reading_start as u32,
            history_reading_len: history.map_or(0, |(reading, _)| reading.len()) as u32,
            history_surface_start: surface_start as u32,
            history_surface_len: history.map_or(0, |(_, surface)| surface.len()) as u32,
        })
    }

    pub fn candidate_count(&self) -> usize {
        self.candidates.len()
    }

    /// Returns whether this output carries a candidate list.
    pub fn has_candidates(&self) -> bool {
        self.has_candidates
    }

    pub fn candidate_kind(&self) -> Option<CandidateKind> {
        self.has_candidates.then_some(self.candidate_kind)
    }

    /// Returns the candidate presentation when a list is present.
    pub fn candidate_presentation(&self) -> Option<CandidatePresentation> {
        self.has_candidates.then_some(self.candidate_presentation)
    }

    /// Returns one candidate without allocating.
    pub fn candidate(&self, index: usize) -> Option<(&str, &str)> {
        let span = *self.candidates.as_slice().get(index)?;
        Some((self.candidate_text(span), self.candidate_annotation(span)))
    }

    /// Returns the exact persisted identity only for a history prediction row.
    /// It is intentionally unavailable through the ordinary `Output` type.
    pub fn candidate_history_identity(&self, index: usize) -> Option<(&str, &str)> {
        let span = *self.candidates.as_slice().get(index)?;
        if span.history_reading_len == 0 || span.history_surface_len == 0 {
            return None;
        }
        let reading = self.candidate_history_readings.as_str().get(
            span.history_reading_start as usize
                ..span.history_reading_start as usize + span.history_reading_len as usize,
        )?;
        let surface = self.candidate_history_surfaces.as_str().get(
            span.history_surface_start as usize
                ..span.history_surface_start as usize + span.history_surface_len as usize,
        )?;
        Some((reading, surface))
    }

    /// Returns whether a candidate carries the private, exact history
    /// capability used by the UI board. This is exposed as a typed display
    /// marker on `Candidate`, while the identity itself stays in-process.
    pub fn candidate_is_deletable_history(&self, index: usize) -> bool {
        self.candidate_history_identity(index).is_some()
    }

    /// The selected candidate index, when a list is present.
    pub fn selected_candidate(&self) -> Option<u16> {
        self.has_candidates.then_some(self.selected_candidate)
    }

    /// The candidate page size, when a list is present.
    pub fn candidate_page_size(&self) -> Option<u16> {
        self.has_candidates.then_some(self.candidate_page_size)
    }

    /// Sets source-backed detail for the selected candidate.
    ///
    /// An invalid, duplicate, or oversized field clears any preceding detail.
    /// This fail-closed rule prevents a newly selected candidate from inheriting
    /// stale explanatory text. Detail cannot outlive the candidate list it
    /// describes.
    pub fn set_candidate_detail(
        &mut self,
        detail: CandidateDetailInput<'_>,
    ) -> Result<(), Overflow> {
        let CandidateDetailInput {
            reading,
            definition,
            definition_truncated,
            aliases,
            related,
            similar,
            antonyms,
        } = detail;
        // A detail describes one particular selected candidate. Never retain a
        // previous one if this replacement cannot be built completely.
        self.clear_candidate_detail();
        if !self.has_candidates
            || reading.is_empty()
            || definition.is_empty()
            || reading.len() > MAX_CANDIDATE_DETAIL_READING_BYTES
            || definition.len() > MAX_CANDIDATE_DETAIL_DEFINITION_BYTES
        {
            return Err(Overflow);
        }
        let groups = [aliases, related, similar, antonyms];
        let mut seen = [""; MAX_CANDIDATE_DETAIL_RELATIONS * 4];
        let mut seen_len = 0;
        let mut total_relation_bytes = 0usize;
        for group in groups {
            if group.len() > MAX_CANDIDATE_DETAIL_RELATIONS {
                return Err(Overflow);
            }
            for value in group {
                if value.is_empty()
                    || value.len() > MAX_CANDIDATE_DETAIL_RELATION_BYTES
                    || seen[..seen_len].contains(value)
                {
                    return Err(Overflow);
                }
                total_relation_bytes = match total_relation_bytes.checked_add(value.len()) {
                    Some(total) if total <= MAX_CANDIDATE_DETAIL_RELATION_TEXT_BYTES => total,
                    _ => return Err(Overflow),
                };
                seen[seen_len] = value;
                seen_len += 1;
            }
        }

        // Every capacity check was performed first, so the following writes
        // cannot fail. Keep `?` as a fail-closed guard against future
        // representation changes.
        self.detail_reading.push_str(reading)?;
        self.detail_definition.push_str(definition)?;
        self.detail_definition_truncated = definition_truncated;
        Self::push_detail_group(
            &mut self.detail_relations,
            &mut self.detail_aliases,
            aliases,
        )?;
        Self::push_detail_group(
            &mut self.detail_relations,
            &mut self.detail_related,
            related,
        )?;
        Self::push_detail_group(
            &mut self.detail_relations,
            &mut self.detail_similar,
            similar,
        )?;
        Self::push_detail_group(
            &mut self.detail_relations,
            &mut self.detail_antonyms,
            antonyms,
        )?;
        self.has_candidate_detail = true;
        Ok(())
    }

    /// Returns selected-candidate detail without allocating or copying text.
    pub fn candidate_detail(&self) -> Option<CandidateDetailRef<'_>> {
        self.has_candidate_detail.then_some(CandidateDetailRef {
            reading: self.detail_reading.as_str(),
            definition: self.detail_definition.as_str(),
            definition_truncated: self.detail_definition_truncated,
            relation_text: self.detail_relations.as_str(),
            aliases: self.detail_aliases.as_slice(),
            related: self.detail_related.as_slice(),
            similar: self.detail_similar.as_slice(),
            antonyms: self.detail_antonyms.as_slice(),
        })
    }

    /// Drops selected-candidate detail while retaining the candidate list.
    pub fn clear_candidate_detail(&mut self) {
        self.has_candidate_detail = false;
        self.detail_reading.clear();
        self.detail_definition.clear();
        self.detail_definition_truncated = false;
        self.detail_relations.clear();
        self.detail_aliases.clear();
        self.detail_related.clear();
        self.detail_similar.clear();
        self.detail_antonyms.clear();
    }

    fn push_detail_group(
        text: &mut FixedStr<MAX_CANDIDATE_DETAIL_RELATION_TEXT_BYTES>,
        spans: &mut FixedVec<DetailSpan, MAX_CANDIDATE_DETAIL_RELATIONS>,
        values: &[&str],
    ) -> Result<(), Overflow> {
        for value in values {
            let start = text.len();
            text.push_str(value)?;
            spans.push(DetailSpan {
                start: start as u32,
                len: value.len() as u32,
            })?;
        }
        Ok(())
    }

    fn detail_text(&self, span: DetailSpan) -> &str {
        let start = span.start as usize;
        self.detail_relations
            .as_str()
            .get(start..start + span.len as usize)
            .unwrap_or("")
    }

    fn owned_detail_group(
        &self,
        spans: &FixedVec<DetailSpan, MAX_CANDIDATE_DETAIL_RELATIONS>,
    ) -> Vec<String> {
        spans
            .as_slice()
            .iter()
            .map(|span| self.detail_text(*span).to_owned())
            .collect()
    }

    fn candidate_text(&self, span: CandidateSpan) -> &str {
        let start = span.text_start as usize;
        let end = start + span.text_len as usize;
        self.candidate_text.as_str().get(start..end).unwrap_or("")
    }

    fn candidate_annotation(&self, span: CandidateSpan) -> &str {
        let start = span.annotation_start as usize;
        let end = start + span.annotation_len as usize;
        self.candidate_annotations
            .as_str()
            .get(start..end)
            .unwrap_or("")
    }

    /// Returns the pushed segment spans, in push order.
    pub fn segments(&self) -> &[SegSpan] {
        self.segments.as_slice()
    }

    fn segment_text(&self, span: SegSpan) -> &str {
        let start = span.start as usize;
        let end = start + span.len as usize;
        // `start`/`end` were produced by `push_segment` as byte offsets
        // into `self.preedit` at whole-`&str`-append boundaries, so this
        // slice is always within bounds and on a char boundary.
        self.preedit.as_str().get(start..end).unwrap_or("")
    }

    fn encode_body<S: Sink>(&self, w: &mut S) -> Result<(), Error> {
        if (self.has_candidate_detail && !self.has_candidates)
            || (self.has_candidates
                && (self.candidates.is_empty()
                    || usize::from(self.selected_candidate) >= self.candidates.len()
                    || self.candidate_page_size == 0))
        {
            return Err(Error::TooLarge);
        }
        w.write_bool(self.consumed)?;
        w.write_bool(self.beep)?;
        w.write_option(&self.mode, |w, m| m.encode(w))?;
        if self.has_preedit {
            w.write_u8(1)?;
            w.write_count(self.segments.len())?;
            for span in self.segments.as_slice() {
                w.write_str(self.segment_text(*span))?;
                span.underline.encode(w)?;
            }
            w.write_u32(self.cursor)?;
        } else {
            w.write_u8(0)?;
        }
        if self.has_commit {
            w.write_u8(1)?;
            w.write_str(self.commit.as_str())?;
        } else {
            w.write_u8(0)?;
        }
        w.write_str(self.delete_before.as_str())?;
        if self.has_candidates {
            w.write_u8(1)?;
            self.candidate_kind.encode(w)?;
            self.candidate_presentation.encode(w)?;
            w.write_count(self.candidates.len())?;
            for span in self.candidates.as_slice() {
                w.write_str(self.candidate_text(*span))?;
                w.write_str(self.candidate_annotation(*span))?;
                w.write_bool(span.history_reading_len != 0 && span.history_surface_len != 0)?;
            }
            w.write_u16(self.selected_candidate)?;
            w.write_u16(self.candidate_page_size)?;
        } else {
            w.write_u8(0)?;
        }
        if self.has_candidate_detail {
            w.write_u8(1)?;
            w.write_str(self.detail_reading.as_str())?;
            w.write_str(self.detail_definition.as_str())?;
            w.write_bool(self.detail_definition_truncated)?;
            for spans in [
                &self.detail_aliases,
                &self.detail_related,
                &self.detail_similar,
                &self.detail_antonyms,
            ] {
                w.write_count(spans.len())?;
                for span in spans.as_slice() {
                    w.write_str(self.detail_text(*span))?;
                }
            }
        } else {
            w.write_u8(0)?;
        }
        Ok(())
    }

    /// Encodes a complete `Response::Output` frame (4-byte length prefix
    /// included) into `dst`, without allocating.
    ///
    /// Returns the number of bytes written. Fails with [`Error::Overflow`]
    /// if `dst` is too small to hold the frame.
    pub fn encode_frame(&self, id: RequestId, dst: &mut [u8]) -> Result<usize, Error> {
        if dst.len() < FRAME_HEADER_LEN {
            return Err(Error::Overflow);
        }
        let body_len = {
            let mut w = SliceSink::new(&mut dst[FRAME_HEADER_LEN..]);
            w.write_u16(PROTOCOL_VERSION)?;
            w.write_u64(id)?;
            w.write_u16(RES_OUTPUT)?;
            self.encode_body(&mut w)?;
            w.len()
        };
        if body_len > MAX_PAYLOAD {
            return Err(Error::TooLarge);
        }
        let len_bytes = (body_len as u32).to_le_bytes();
        dst[..FRAME_HEADER_LEN].copy_from_slice(&len_bytes);
        Ok(FRAME_HEADER_LEN + body_len)
    }

    /// Builds an owned [`Output`] equivalent to this buffer's contents.
    ///
    /// This allocates (a `Vec<Segment>` plus one `String` per segment and
    /// one for the commit text) and is meant for tests and the DLL side,
    /// not the engine's hot path.
    pub fn to_output(&self) -> Output {
        let preedit = if self.has_preedit {
            let mut segments = Vec::with_capacity(self.segments.len());
            for span in self.segments.as_slice() {
                segments.push(Segment {
                    text: self.segment_text(*span).to_string(),
                    underline: span.underline,
                });
            }
            Some(Preedit {
                segments,
                cursor: self.cursor,
            })
        } else {
            None
        };
        let commit = self.commit_text().map(|s| s.to_string());
        let candidates = if self.has_candidates {
            Some(CandidateList {
                kind: self.candidate_kind,
                presentation: self.candidate_presentation,
                items: self
                    .candidates
                    .as_slice()
                    .iter()
                    .map(|span| Candidate {
                        text: self.candidate_text(*span).to_string(),
                        annotation: self.candidate_annotation(*span).to_string(),
                        deletable_history: span.history_reading_len != 0
                            && span.history_surface_len != 0,
                    })
                    .collect(),
                selected: self.selected_candidate,
                page_size: self.candidate_page_size,
            })
        } else {
            None
        };
        let candidate_detail = self.has_candidate_detail.then(|| CandidateDetail {
            reading: self.detail_reading.as_str().to_owned(),
            definition: self.detail_definition.as_str().to_owned(),
            definition_truncated: self.detail_definition_truncated,
            aliases: self.owned_detail_group(&self.detail_aliases),
            related: self.owned_detail_group(&self.detail_related),
            similar: self.owned_detail_group(&self.detail_similar),
            antonyms: self.owned_detail_group(&self.detail_antonyms),
        });
        Output {
            consumed: self.consumed,
            beep: self.beep,
            mode: self.mode,
            preedit,
            commit,
            delete_before: self.delete_before.as_str().to_owned(),
            candidates,
            candidate_detail,
        }
    }
}

impl Default for OutputBuf {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for OutputBuf {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OutputBuf")
            .field("consumed", &self.consumed)
            .field("beep", &self.beep)
            .field("mode", &self.mode)
            .field("preedit_text", &self.preedit_text())
            .field("segments", &self.segments())
            .field("cursor", &self.cursor)
            .field("commit_text", &self.commit_text())
            .field("candidate_count", &self.candidate_count())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::decode_response;
    use crate::message::Response;

    #[test]
    fn new_is_empty() {
        let buf = OutputBuf::new();
        assert!(!buf.consumed);
        assert!(!buf.beep);
        assert_eq!(buf.mode, None);
        assert_eq!(buf.preedit_text(), "");
        assert_eq!(buf.commit_text(), None);
        assert_eq!(buf.delete_before(), "");
        assert!(buf.segments().is_empty());
        assert_eq!(buf.candidate_count(), 0);
    }

    #[test]
    fn boxed_constructor_has_the_same_empty_state_as_the_value_constructor() {
        let mut buf = OutputBuf::new_boxed();
        assert!(!buf.consumed);
        assert!(!buf.beep);
        assert_eq!(buf.mode, None);
        assert_eq!(buf.preedit_text(), "");
        assert_eq!(buf.commit_text(), None);
        assert_eq!(buf.delete_before(), "");
        assert!(buf.segments().is_empty());
        assert_eq!(buf.candidate_count(), 0);

        // The setup allocation is outside the key path. Reusing the boxed
        // value must preserve the normal allocation-free mutation contract.
        buf.begin_preedit();
        buf.push_segment("a", UnderlineKind::Raw)
            .expect("boxed buffer accepts a segment");
        buf.clear();
        assert_eq!(buf.preedit_text(), "");
    }

    #[test]
    fn push_segment_accumulates_text_and_spans() {
        let mut buf = OutputBuf::new();
        buf.begin_preedit();
        buf.push_segment("かん", UnderlineKind::Raw).expect("push");
        buf.push_segment("字", UnderlineKind::Converted)
            .expect("push");
        buf.set_cursor(3);
        assert_eq!(buf.preedit_text(), "かん字");
        assert_eq!(buf.segments().len(), 2);
        assert_eq!(buf.segments()[0].underline, UnderlineKind::Raw);
        assert_eq!(buf.segments()[1].underline, UnderlineKind::Converted);
    }

    #[test]
    fn push_segment_overflow_is_atomic() {
        let mut buf = OutputBuf::new();
        buf.begin_preedit();
        let long = "a".repeat(MAX_PREEDIT_BYTES);
        buf.push_segment(&long, UnderlineKind::Raw)
            .expect("fits exactly");
        let before = buf.preedit_text().len();
        let result = buf.push_segment("x", UnderlineKind::Raw);
        assert_eq!(result, Err(Overflow));
        assert_eq!(buf.preedit_text().len(), before);
        assert_eq!(buf.segments().len(), 1);
    }

    #[test]
    fn push_segment_rejects_beyond_max_segments() {
        let mut buf = OutputBuf::new();
        buf.begin_preedit();
        for _ in 0..MAX_SEGMENTS {
            buf.push_segment("a", UnderlineKind::Raw).expect("push");
        }
        assert_eq!(buf.push_segment("a", UnderlineKind::Raw), Err(Overflow));
        assert_eq!(buf.segments().len(), MAX_SEGMENTS);
    }

    #[test]
    fn set_commit_overflow_is_atomic() {
        let mut buf = OutputBuf::new();
        buf.set_commit("hello").expect("fits");
        let too_long = "a".repeat(MAX_COMMIT_BYTES + 1);
        assert_eq!(buf.set_commit(&too_long), Err(Overflow));
        // Previous commit text is unchanged.
        assert_eq!(buf.commit_text(), Some("hello"));
    }

    #[test]
    fn append_commit_preserves_an_existing_commit() {
        let mut buf = OutputBuf::new();
        buf.set_commit("かな").expect("fits");
        buf.append_commit("A").expect("fits");
        assert_eq!(buf.commit_text(), Some("かなA"));
    }

    #[test]
    fn clear_resets_everything() {
        let mut buf = OutputBuf::new();
        buf.consumed = true;
        buf.beep = true;
        buf.mode = Some(Mode::Hiragana);
        buf.begin_preedit();
        buf.push_segment("a", UnderlineKind::Raw).expect("push");
        buf.set_commit("b").expect("push");
        buf.set_delete_before("committed").expect("delete text");
        buf.begin_candidates(0, 9).expect("begin candidates");
        buf.push_candidate("候補", "注釈").expect("push candidate");
        buf.clear();
        assert!(!buf.consumed);
        assert!(!buf.beep);
        assert_eq!(buf.mode, None);
        assert_eq!(buf.preedit_text(), "");
        assert_eq!(buf.commit_text(), None);
        assert_eq!(buf.delete_before(), "");
        assert!(buf.segments().is_empty());
        assert_eq!(buf.candidate_count(), 0);
    }

    #[test]
    fn encode_frame_matches_to_output_through_decode() {
        let mut buf = OutputBuf::new();
        buf.consumed = true;
        buf.mode = Some(Mode::Katakana);
        buf.begin_preedit();
        buf.push_segment("ドッカー", UnderlineKind::Converted)
            .expect("push");
        buf.set_cursor(4);
        buf.set_commit("🍣").expect("push");
        buf.set_delete_before("🍣").expect("delete text");

        let mut frame = [0u8; 256];
        let n = buf.encode_frame(99, &mut frame).expect("encode");
        let (id, response) = decode_response(&frame[FRAME_HEADER_LEN..n]).expect("decode");
        assert_eq!(id, 99);
        assert_eq!(response, Response::Output(buf.to_output()));
    }

    #[test]
    fn candidates_roundtrip_without_allocating_in_the_builder() {
        let mut buf = OutputBuf::new();
        buf.consumed = true;
        buf.begin_candidates(1, 9).expect("begin candidates");
        buf.push_candidate("かな", "ひらがな").expect("push");
        buf.push_candidate("仮名", "IT用語").expect("push");

        let mut frame = [0u8; 256];
        let n = buf.encode_frame(17, &mut frame).expect("encode");
        let (id, response) = decode_response(&frame[FRAME_HEADER_LEN..n]).expect("decode");
        assert_eq!(id, 17);
        assert_eq!(response, Response::Output(buf.to_output()));

        let candidates = buf.to_output().candidates.expect("candidate list");
        assert_eq!(candidates.kind, CandidateKind::Conversion);
        assert_eq!(candidates.presentation, CandidatePresentation::Compact);
        assert_eq!(candidates.selected, 1);
        assert_eq!(candidates.page_size, 9);
        assert_eq!(candidates.items[1].text, "仮名");
        assert_eq!(candidates.items[1].annotation, "IT用語");
    }

    #[test]
    fn history_candidate_marker_roundtrips_but_private_learning_key_never_reaches_output_wire() {
        let mut buf = OutputBuf::new();
        buf.begin_suggestions(0, 9).expect("suggestions");
        buf.push_history_candidate(
            "visible surface",
            "history label",
            "private-reading-7b4c",
            "private-surface-09e1",
        )
        .expect("history candidate");
        assert_eq!(
            buf.candidate_history_identity(0),
            Some(("private-reading-7b4c", "private-surface-09e1"))
        );
        assert!(buf.candidate_is_deletable_history(0));

        let mut frame = vec![0; crate::MAX_FRAME];
        let written = buf.encode_frame(77, &mut frame).expect("encode");
        let payload = &frame[crate::FRAME_HEADER_LEN..written];
        assert!(!payload
            .windows("private-reading-7b4c".len())
            .any(|bytes| bytes == b"private-reading-7b4c"));
        assert!(!payload
            .windows("private-surface-09e1".len())
            .any(|bytes| bytes == b"private-surface-09e1"));
        let (_, response) = crate::decode_response(payload).expect("decode");
        let crate::Response::Output(output) = response else {
            panic!("expected output response");
        };
        assert!(output.candidates.expect("candidates").items[0].deletable_history);
    }

    #[test]
    fn selected_candidate_detail_roundtrips_and_clear_is_complete() {
        let mut buf = OutputBuf::new();
        buf.begin_candidates(0, 9).expect("begin candidates");
        buf.push_candidate("Rust", "language")
            .expect("push candidate");
        buf.set_candidate_detail(CandidateDetailInput {
            reading: "らすと",
            definition: "安全性と速度を重視するプログラミング言語。",
            definition_truncated: false,
            aliases: &["Rust language"],
            related: &["Cargo"],
            similar: &["C++"],
            antonyms: &["unsafe"],
        })
        .expect("detail");

        let detail = buf.candidate_detail().expect("borrowed detail");
        assert_eq!(detail.reading, "らすと");
        assert_eq!(
            detail.definition,
            "安全性と速度を重視するプログラミング言語。"
        );
        assert!(!detail.definition_truncated);
        assert!(detail.aliases().eq(["Rust language"]));
        assert!(detail.related().eq(["Cargo"]));
        assert!(detail.similar().eq(["C++"]));
        assert!(detail.antonyms().eq(["unsafe"]));

        let mut frame = [0u8; 4096];
        let len = buf.encode_frame(19, &mut frame).expect("encode");
        let (_, decoded) = decode_response(&frame[FRAME_HEADER_LEN..len]).expect("decode");
        assert_eq!(decoded, Response::Output(buf.to_output()));
        buf.clear();
        assert!(buf.candidate_detail().is_none());
    }

    #[test]
    fn selected_candidate_detail_failure_clears_stale_detail() {
        let mut buf = OutputBuf::new();
        buf.begin_candidates(0, 9).expect("begin candidates");
        buf.push_candidate("Rust", "").expect("push candidate");
        buf.set_candidate_detail(CandidateDetailInput {
            reading: "reading",
            definition: "definition",
            definition_truncated: false,
            aliases: &["one"],
            related: &[],
            similar: &[],
            antonyms: &[],
        })
        .expect("detail");
        assert!(buf.candidate_detail().is_some());

        // A new selected index starts a new candidate snapshot. The prior
        // detail must be gone before attempting to attach B's detail.
        buf.begin_candidates(1, 9).expect("new selected candidate");
        buf.push_candidate("A", "").expect("push A");
        buf.push_candidate("B", "").expect("push B");
        assert!(buf.candidate_detail().is_none());

        let too_long = "x".repeat(MAX_CANDIDATE_DETAIL_READING_BYTES + 1);
        assert_eq!(
            buf.set_candidate_detail(CandidateDetailInput {
                reading: &too_long,
                definition: "definition",
                definition_truncated: false,
                aliases: &[],
                related: &[],
                similar: &[],
                antonyms: &[],
            }),
            Err(Overflow)
        );
        assert!(buf.candidate_detail().is_none());
        buf.set_candidate_detail(CandidateDetailInput {
            reading: "new-reading",
            definition: "new definition",
            definition_truncated: false,
            aliases: &["two"],
            related: &[],
            similar: &[],
            antonyms: &[],
        })
        .expect("replacement detail");
        assert_eq!(
            buf.set_candidate_detail(CandidateDetailInput {
                reading: "reading",
                definition: "definition",
                definition_truncated: false,
                aliases: &["same"],
                related: &["same"],
                similar: &[],
                antonyms: &[],
            }),
            Err(Overflow)
        );
        assert!(buf.candidate_detail().is_none());
    }

    #[test]
    fn selected_candidate_detail_enforces_each_field_boundary() {
        let mut buf = OutputBuf::new();
        buf.begin_candidates(0, 9).expect("begin candidates");
        buf.push_candidate("Rust", "").expect("push candidate");
        let reading = "r".repeat(MAX_CANDIDATE_DETAIL_READING_BYTES);
        let definition = "d".repeat(MAX_CANDIDATE_DETAIL_DEFINITION_BYTES);
        let relation = "a".repeat(MAX_CANDIDATE_DETAIL_RELATION_BYTES);
        buf.set_candidate_detail(CandidateDetailInput {
            reading: &reading,
            definition: &definition,
            definition_truncated: false,
            aliases: &[&relation],
            related: &[],
            similar: &[],
            antonyms: &[],
        })
        .expect("all exact bounds fit");
        assert!(buf.candidate_detail().is_some());

        let too_long_relation = "a".repeat(MAX_CANDIDATE_DETAIL_RELATION_BYTES + 1);
        assert_eq!(
            buf.set_candidate_detail(CandidateDetailInput {
                reading: "reading",
                definition: "definition",
                definition_truncated: false,
                aliases: &[&too_long_relation],
                related: &[],
                similar: &[],
                antonyms: &[],
            }),
            Err(Overflow)
        );
        assert!(buf.candidate_detail().is_none());
    }

    #[test]
    fn selected_candidate_detail_preserves_explicit_definition_truncation() {
        let source = "あ".repeat(342);
        let (preview, truncated) = CandidateDetail::bounded_definition_preview(&source);
        assert!(truncated);
        let mut buf = OutputBuf::new();
        buf.begin_candidates(0, 9).expect("begin candidates");
        buf.push_candidate("Rust", "").expect("push candidate");
        buf.set_candidate_detail(CandidateDetailInput {
            reading: "reading",
            definition: preview,
            definition_truncated: truncated,
            aliases: &[],
            related: &[],
            similar: &[],
            antonyms: &[],
        })
        .expect("detail preview");

        let mut frame = [0u8; 2048];
        let len = buf.encode_frame(20, &mut frame).expect("encode");
        let (_, decoded) = decode_response(&frame[FRAME_HEADER_LEN..len]).expect("decode");
        let Response::Output(output) = decoded else {
            panic!("expected output");
        };
        assert_eq!(
            output.candidate_detail.expect("detail"),
            CandidateDetail {
                reading: "reading".to_owned(),
                definition: preview.to_owned(),
                definition_truncated: true,
                aliases: Vec::new(),
                related: Vec::new(),
                similar: Vec::new(),
                antonyms: Vec::new(),
            }
        );
    }

    #[test]
    fn expanded_conversion_candidates_roundtrip_with_their_presentation() {
        let mut buf = OutputBuf::new();
        buf.consumed = true;
        buf.begin_conversion_candidates(CandidatePresentation::Expanded, 1, 9)
            .expect("begin expanded candidates");
        buf.push_candidate("first", "").expect("push");
        buf.push_candidate("second", "").expect("push");

        assert_eq!(buf.candidate_kind(), Some(CandidateKind::Conversion));
        assert_eq!(
            buf.candidate_presentation(),
            Some(CandidatePresentation::Expanded)
        );

        let mut frame = [0u8; 256];
        let n = buf.encode_frame(18, &mut frame).expect("encode");
        let (id, response) = decode_response(&frame[FRAME_HEADER_LEN..n]).expect("decode");
        assert_eq!(id, 18);
        assert_eq!(response, Response::Output(buf.to_output()));
    }

    #[test]
    fn candidate_selection_must_reference_a_pushed_item() {
        let mut buf = OutputBuf::new();
        buf.begin_candidates(1, 9).expect("begin candidates");
        buf.push_candidate("一件だけ", "").expect("push");

        let mut frame = [0u8; 128];
        assert_eq!(buf.encode_frame(1, &mut frame), Err(Error::TooLarge));
    }

    #[test]
    fn encode_frame_reports_overflow_for_small_dst() {
        let buf = OutputBuf::new();
        let mut tiny = [0u8; 2];
        assert_eq!(buf.encode_frame(1, &mut tiny), Err(Error::Overflow));
    }

    #[test]
    fn default_matches_new() {
        let a = OutputBuf::default();
        let b = OutputBuf::new();
        assert_eq!(a.to_output(), b.to_output());
    }

    #[test]
    fn debug_impl_does_not_panic() {
        let buf = OutputBuf::new();
        let s = format!("{buf:?}");
        assert!(s.contains("OutputBuf"));
    }
}
