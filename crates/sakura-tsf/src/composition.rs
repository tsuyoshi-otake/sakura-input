//! The composition — the underlined, not-yet-committed text in the document.
//!
//! Everything here runs inside a document lock (see [`crate::edit_session`]) and
//! assumes `ec` is a valid read/write edit cookie. Nothing here decides *what*
//! the preedit should say; it only makes the document agree with a string that
//! was decided outside the lock.
//!
//! Keeping the in-memory preedit as the source of truth and the document as a
//! projection of it is what lets an asynchronous lock arrive late without
//! producing a document that disagrees with what the user typed.

use core::mem::ManuallyDrop;

use windows::Win32::Foundation::{E_UNEXPECTED, RECT};
use windows::Win32::UI::TextServices::{
    ITfCategoryMgr, ITfComposition, ITfCompositionSink, ITfContext, ITfContextComposition,
    ITfProperty, ITfRange, GUID_PROP_ATTRIBUTE, TF_AE_NONE, TF_ANCHOR_END, TF_DEFAULT_SELECTION,
    TF_SELECTION, TF_SELECTIONSTYLE, TS_E_NOLAYOUT,
};
use windows_core::{Error, Interface, Result, GUID};

use sakura_proto::{ScreenRect, Segment, UnderlineKind};
use sakura_reg::{
    GUID_DISPLAY_ATTRIBUTE_CONVERTED, GUID_DISPLAY_ATTRIBUTE_FOCUSED, GUID_DISPLAY_ATTRIBUTE_RAW,
};

/// Terminal result of one composition-geometry query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryResult {
    Ready(ScreenRect),
    /// The host has not laid out the range yet. The layout sink owns the one
    /// future retry; callers must not poll immediately.
    NoLayout,
    /// There is no usable composition/view/rectangle to anchor to.
    Unavailable,
}

/// Queries the active composition range in screen coordinates.
pub fn candidate_rect<Authority>(
    context: &ITfContext,
    ec: u32,
    handle: Option<&ITfComposition>,
    authority: &mut Authority,
) -> GeometryResult
where
    Authority: FnMut() -> Result<()>,
{
    let Some(composition) = handle else {
        return GeometryResult::Unavailable;
    };
    // Each individual host call can re-enter a focus/context/lifecycle path.
    // The sequencing helper checks authority before *and* after every call, so
    // an invalidation during GetRange cannot continue into GetActiveView or
    // GetTextExt, and an invalidation during GetActiveView cannot reach
    // GetTextExt.
    query_candidate_rect_host_calls(
        authority,
        // SAFETY: the retained composition belongs to `context` and `ec` is
        // the current read or read/write cookie.
        || unsafe { composition.GetRange() },
        // SAFETY: `context` is the live edit-session context while authority
        // grants this callback permission to inspect its active view.
        || unsafe { context.GetActiveView() },
        // SAFETY: the range and view were obtained from this current context,
        // and `ec` remains valid for the edit session that owns this query.
        |view, range| unsafe {
            let mut rect = RECT::default();
            let mut clipped = false.into();
            view.GetTextExt(ec, range, &mut rect, &mut clipped)?;
            Ok(rect)
        },
    )
}

/// What the document should be made to show.
///
/// The text travels by value because the edit that applies it may run long after
/// the keystroke that produced it, by which time the live preedit has moved on.
#[derive(Clone, Debug)]
pub enum Update {
    /// Show these segments as an active, underlined preedit. Each segment
    /// carries its own [`UnderlineKind`], and gets its own display-attribute
    /// range in the document so the host draws unconverted input, a
    /// converted clause, and the one clause currently focused differently
    /// -- see [`write_text`].
    Show(Vec<Segment>),
    /// Show this and then hand it to the application as ordinary text.
    Commit(String),
    /// Throw the preedit away without committing anything.
    Discard,
    /// Remove the exact committed surface immediately before the caret.
    /// Commit undo applies this immediately before reopening the restored
    /// composition; the host range is independently verified in the edit
    /// session rather than trusting a length-only payload.
    DeleteBefore(String),
}

/// Whether `segments` has anything a user would see. An empty segment list,
/// or one made only of empty-text segments, is not something a zero-width
/// composition should be opened (or kept open) for -- the same rule
/// `Update::Show(String)` used to apply to one flattened string now has to
/// look through every segment for.
fn segments_have_text(segments: &[Segment]) -> bool {
    segments.iter().any(|segment| !segment.text.is_empty())
}

/// Whether an exact-text undo failure happened before the first document
/// mutation or after a host call that may have changed the document.
#[derive(Debug)]
pub(crate) enum DeleteBeforeError {
    Validation(Error),
    Mutation(Error),
}

impl DeleteBeforeError {
    fn into_error(self) -> Error {
        match self {
            Self::Validation(error) | Self::Mutation(error) => error,
        }
    }
}

/// Everything the document-side operations need, gathered once per edit.
#[derive(Clone, Debug)]
pub struct DocumentEdit {
    pub context: ITfContext,
    pub sink: ITfCompositionSink,
    /// Absent when the category manager could not be created. The preedit is
    /// then drawn without an underline rather than not drawn at all — losing the
    /// styling is a cosmetic failure, losing the text is a data-loss one.
    pub category_mgr: Option<ITfCategoryMgr>,
}

/// Makes the document match `update` while bracketing only the final `EndComposition` COM
/// call. The callbacks deliberately do not cover preceding `SetText`, display
/// attribute, or caret calls: an external `OnCompositionTerminated` re-entering
/// from any of those calls must remain an external lifecycle boundary.
///
/// `handle` is updated in place rather than returned so that a failure partway
/// through still leaves the caller holding whatever composition really exists.
/// Dropping a live `ITfComposition` on the error path would strand an underlined
/// run in the user's document that nothing can ever end.
pub fn apply_with_end_composition_callbacks<Authority, BeforeEnd, AfterEnd>(
    edit: &DocumentEdit,
    ec: u32,
    handle: &mut Option<ITfComposition>,
    update: &Update,
    authority: &mut Authority,
    mut before_end: BeforeEnd,
    mut after_end: AfterEnd,
) -> Result<()>
where
    Authority: FnMut() -> Result<()>,
    BeforeEnd: FnMut(&ITfComposition) -> Result<()>,
    AfterEnd: FnMut(&ITfComposition) -> Result<()>,
{
    authority()?;
    match update {
        Update::Show(segments) if segments_have_text(segments) => {
            if handle.is_none() {
                start(edit, ec, handle, authority)?;
            }
            let Some(composition) = handle.as_ref() else {
                return Ok(());
            };
            write_text(edit, ec, composition, segments, authority)
        }

        // An empty preedit is not something the user can see, and a zero-width
        // composition left in the document swallows the caret in some hosts.
        Update::Show(_) | Update::Discard => match handle.take() {
            Some(composition) => {
                clear_and_end(ec, &composition, authority, &mut before_end, &mut after_end)
            }
            None => Ok(()),
        },

        Update::Commit(text) => match handle.take() {
            Some(composition) => {
                if text.is_empty() {
                    return clear_and_end(
                        ec,
                        &composition,
                        authority,
                        &mut before_end,
                        &mut after_end,
                    );
                }
                // A commit has no segmentation of its own -- `Output::commit`
                // is a plain `String` -- so it is written as one RAW segment,
                // matching what every write drew before per-segment
                // underlines existed. The attribute is moot the instant
                // `EndComposition` below turns this back into ordinary text,
                // but writing through the same `write_text` path keeps this
                // arm from duplicating its `SetText`/caret logic.
                let committed_segment = [Segment {
                    text: text.clone(),
                    underline: UnderlineKind::Raw,
                }];
                write_text(edit, ec, &composition, &committed_segment, authority)?;
                end_composition(ec, &composition, authority, &mut before_end, &mut after_end)
            }
            // Committing without an open composition is not an error: the host
            // may have terminated ours (OnCompositionTerminated) between the
            // keystroke and this lock. The text still belongs in the document.
            None if !text.is_empty() => insert_plain(edit, ec, text, authority),
            None => Ok(()),
        },

        Update::DeleteBefore(expected) => {
            if handle.is_some() || expected.is_empty() {
                return Err(windows_core::Error::new(
                    E_UNEXPECTED,
                    "commit undo requires an idle, non-empty document range",
                ));
            }
            delete_before_caret(edit, ec, expected, authority)
                .map_err(DeleteBeforeError::into_error)
        }
    }
}

/// Executes one potentially re-entrant COM operation only while the queued
/// write still owns the document. A lifecycle callback can invalidate that
/// ownership *during* any host call, so the post-call check is as important as
/// the pre-call check: it prevents the next document or UI operation from
/// running under an old ticket.
fn checked_host_call<T, Authority, Call>(authority: &mut Authority, call: Call) -> Result<T>
where
    Authority: FnMut() -> Result<()>,
    Call: FnOnce() -> Result<T>,
{
    authority()?;
    let call_result = call();
    let post_call_authority = authority();
    match (call_result, post_call_authority) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

/// Runs the three host calls needed for candidate geometry as one authority-
/// gated sequence. It is intentionally separate from [`candidate_rect`] so the
/// exact call ordering can be fault-injection tested without creating COM
/// objects. Every call still passes through [`checked_host_call`]; this helper
/// only prevents a later geometry call from following a re-entrant invalidation
/// in an earlier one.
fn query_candidate_rect_host_calls<Range, View, Authority, GetRange, GetActiveView, GetTextExt>(
    authority: &mut Authority,
    get_range: GetRange,
    get_active_view: GetActiveView,
    get_text_ext: GetTextExt,
) -> GeometryResult
where
    Authority: FnMut() -> Result<()>,
    GetRange: FnOnce() -> Result<Range>,
    GetActiveView: FnOnce() -> Result<View>,
    GetTextExt: FnOnce(&View, &Range) -> Result<RECT>,
{
    let queried = (|| {
        let range = checked_host_call(authority, get_range)?;
        let view = checked_host_call(authority, get_active_view)?;
        checked_host_call(authority, || get_text_ext(&view, &range))
    })();
    match queried {
        Ok(rect) => {
            let rect = ScreenRect {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            };
            if rect.is_valid() {
                GeometryResult::Ready(rect)
            } else {
                GeometryResult::Unavailable
            }
        }
        Err(error) if error.code() == TS_E_NOLAYOUT => GeometryResult::NoLayout,
        // This includes a failed authority gate. A caller that supplied a
        // lease authority rechecks it before completing the query and abandons
        // the claimed geometry state when the lease went stale.
        Err(_) => GeometryResult::Unavailable,
    }
}

pub(crate) fn delete_before_caret<Authority>(
    edit: &DocumentEdit,
    ec: u32,
    expected: &str,
    authority: &mut Authority,
) -> core::result::Result<(), DeleteBeforeError>
where
    Authority: FnMut() -> Result<()>,
{
    // Start from the real selection. Asking a host to synthesize a hypothetical
    // insertion range has crashed Windows' TextInputFramework in Electron text
    // stores, so every document mutation in this module uses an owned range that
    // came from the context itself.
    let range = current_selection_range(&edit.context, ec, authority)
        .map_err(DeleteBeforeError::Validation)?;
    let expected_wide: Vec<u16> = expected.encode_utf16().collect();
    let units = i32::try_from(expected_wide.len()).unwrap_or(i32::MAX);
    delete_before_host_calls(
        &expected_wide,
        authority,
        // SAFETY: `range` belongs to this context and `ec` is the live edit
        // cookie under which the range was obtained.
        || unsafe { range.IsEmpty(ec).map(|value| value.as_bool()) },
        // SAFETY: the same owned range and edit cookie remain valid for the
        // duration of this synchronous host call.
        || unsafe { range.Collapse(ec, TF_ANCHOR_END) },
        |shifted| unsafe {
            // SAFETY: `range` remains owned by this live context and edit
            // cookie; `shifted` is writable for the call and the null halt
            // position requests TSF's normal context boundary.
            range.ShiftStart(ec, -units, shifted, core::ptr::null())
        },
        |actual, copied| {
            // SAFETY: `actual` and `copied` are writable buffers owned by the
            // caller, while `range` and `ec` remain valid as above.
            unsafe { range.GetText(ec, 0, actual, copied) }
        },
        || {
            // SAFETY: the verified range is still owned by this context and
            // the empty slice requests deletion under the live edit cookie.
            unsafe { range.SetText(ec, 0, &[]) }
        },
    )
}

/// Runs the exact-text undo decision and mutation sequence after the real
/// selection range has been acquired. Keeping this seam independent of COM
/// makes every rejection boundary fault-injectable while the production path
/// still supplies the range returned by [`ITfContext::GetSelection`].
fn delete_before_host_calls<Authority, IsEmpty, Collapse, ShiftStart, GetText, SetText>(
    expected: &[u16],
    authority: &mut Authority,
    is_empty: IsEmpty,
    collapse: Collapse,
    shift_start: ShiftStart,
    get_text: GetText,
    set_text: SetText,
) -> core::result::Result<(), DeleteBeforeError>
where
    Authority: FnMut() -> Result<()>,
    IsEmpty: FnOnce() -> Result<bool>,
    Collapse: FnOnce() -> Result<()>,
    ShiftStart: FnOnce(&mut i32) -> Result<()>,
    GetText: FnOnce(&mut [u16], &mut u32) -> Result<()>,
    SetText: FnOnce() -> Result<()>,
{
    let units = i32::try_from(expected.len()).unwrap_or(i32::MAX);
    if units <= 0 {
        return Err(DeleteBeforeError::Validation(Error::new(
            E_UNEXPECTED,
            "commit undo requires non-empty committed text",
        )));
    }

    let is_empty = checked_host_call(authority, is_empty).map_err(DeleteBeforeError::Validation)?;
    if !is_empty {
        return Err(DeleteBeforeError::Validation(Error::new(
            E_UNEXPECTED,
            "commit undo requires a collapsed host selection",
        )));
    }

    checked_host_call(authority, collapse).map_err(DeleteBeforeError::Validation)?;

    let mut shifted = 0i32;
    checked_host_call(authority, || shift_start(&mut shifted))
        .map_err(DeleteBeforeError::Validation)?;
    if shifted != -units {
        return Err(DeleteBeforeError::Validation(Error::new(
            E_UNEXPECTED,
            "document no longer contains the commit immediately before the caret",
        )));
    }

    let mut actual = vec![0u16; expected.len().saturating_add(1)];
    let mut copied = 0u32;
    checked_host_call(authority, || get_text(&mut actual, &mut copied))
        .map_err(DeleteBeforeError::Validation)?;
    let copied = usize::try_from(copied).unwrap_or(usize::MAX);
    if copied != expected.len() || actual.get(..copied) != Some(expected) {
        return Err(DeleteBeforeError::Validation(Error::new(
            E_UNEXPECTED,
            "document text before caret does not match the committed undo surface",
        )));
    }

    // SetText is the first potentially mutating call. Its pre-call authority
    // check makes a stale ticket a definite rejection; the post-call check
    // deliberately classifies a re-entrant invalidation as Mutation/Unknown.
    authority().map_err(DeleteBeforeError::Validation)?;
    let set_result = set_text();
    let post_call_authority = authority();
    match (set_result, post_call_authority) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), _) | (Ok(()), Err(error)) => Err(DeleteBeforeError::Mutation(error)),
    }
}

/// Opens a composition at the caret. The new handle is installed before the
/// post-call authority check: `StartComposition` may have succeeded and then
/// synchronously re-entered a lifecycle callback, so dropping it on that branch
/// would strand a live host composition.
fn start<Authority>(
    edit: &DocumentEdit,
    ec: u32,
    handle: &mut Option<ITfComposition>,
    authority: &mut Authority,
) -> Result<()>
where
    Authority: FnMut() -> Result<()>,
{
    // Use the context's real selection instead of asking the host to synthesize
    // an insertion range. Besides matching the replacement semantics users
    // expect for a non-empty selection, this avoids the TextInputFramework crash
    // observed in VS Code Stable's Electron text store.
    let range = current_selection_range(&edit.context, ec, authority)?;

    let composition: ITfContextComposition = checked_host_call(authority, || edit.context.cast())?;
    // SAFETY: `range` belongs to this context under the same edit cookie and
    // `sink` outlives the composition — it is the text service itself.
    authority()?;
    // SAFETY: `range` belongs to this context under the same edit cookie and
    // `sink` is this live text-service object for the duration of the call.
    let started = unsafe { composition.StartComposition(ec, &range, &edit.sink) };
    match started {
        Ok(started) => {
            *handle = Some(started);
            authority()
        }
        Err(error) => {
            // A failed COM call can still have re-entered a lifecycle callback,
            // so observe its post-call authority before reporting the HRESULT.
            let _ = authority();
            Err(error)
        }
    }
}

/// Returns the context's current selection as one owned range.
pub(crate) fn current_selection_range<Authority>(
    context: &ITfContext,
    ec: u32,
    authority: &mut Authority,
) -> Result<ITfRange>
where
    Authority: FnMut() -> Result<()>,
{
    checked_host_call(authority, || {
        let mut selection = TF_SELECTION::default();
        let mut fetched = 0u32;

        // SAFETY: the output slice contains one fully initialized TF_SELECTION,
        // `fetched` is writable, and `ec` is the active edit cookie for
        // `context`.
        let result = unsafe {
            context.GetSelection(
                ec,
                TF_DEFAULT_SELECTION,
                core::slice::from_mut(&mut selection),
                &mut fetched,
            )
        };

        // SAFETY: `selection.range` started as `None`; GetSelection either left
        // it that way or wrote one owned COM reference. Taking it here transfers
        // that reference exactly once on both the success and error paths. The
        // source is ManuallyDrop, so it cannot be released a second time with
        // `selection`.
        let range = unsafe { ManuallyDrop::take(&mut selection.range) };
        result?;

        if fetched != 1 {
            return Err(windows_core::Error::new(
                E_UNEXPECTED,
                "TSF did not return exactly one current selection",
            ));
        }
        range.ok_or_else(|| {
            windows_core::Error::new(E_UNEXPECTED, "TSF returned a selection without a range")
        })
    })
}

/// One segment's span in the UTF-16 buffer [`write_text`] sends to
/// `SetText`, alongside the underline it should be styled with. `start`/`end`
/// are UTF-16 code-unit offsets from the start of the range that was just
/// written -- what `ITfRange::ShiftStart`/`ShiftEnd` count positions in,
/// since TSF's backing store is UTF-16 just like the buffer this crate hands
/// it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SegmentRange {
    start: u32,
    end: u32,
    underline: UnderlineKind,
}

/// Replaces the composition's contents and puts the caret after them.
///
/// Each segment gets its own display-attribute range so a host that draws
/// underlines can distinguish unconverted input from a converted clause from
/// the one clause currently focused -- the same visual vocabulary Microsoft
/// IME uses to show the user what Space and the arrow keys act on.
fn write_text<Authority>(
    edit: &DocumentEdit,
    ec: u32,
    composition: &ITfComposition,
    segments: &[Segment],
    authority: &mut Authority,
) -> Result<()>
where
    Authority: FnMut() -> Result<()>,
{
    // SAFETY: the composition is live and `ec` is this session's write cookie.
    let range = checked_host_call(authority, || unsafe { composition.GetRange() })?;

    // UTF-16 at the TSF boundary (DESIGN 5.2). A `Vec` rather than a stack
    // buffer because a preedit has no fixed maximum length.
    let (wide, spans) = segment_ranges(segments);
    // SAFETY: `wide` outlives the call and its length is passed with it.
    checked_host_call(authority, || unsafe { range.SetText(ec, 0, &wide) })?;

    apply_display_attributes(edit, ec, &range, &spans, authority)?;
    move_caret_to_end(edit, ec, &range, authority)
}

/// Encodes `segments` as one UTF-16 buffer and records each segment's
/// `[start, end)` span within it, without touching COM at all.
///
/// Kept separate from [`write_text`] so the offset arithmetic -- surrogate
/// pairs included, since a preedit can contain any Unicode scalar value the
/// engine's dictionaries produce -- is directly testable against known text
/// instead of only reachable through a live TSF range.
fn segment_ranges(segments: &[Segment]) -> (Vec<u16>, Vec<SegmentRange>) {
    let mut wide: Vec<u16> = Vec::new();
    let mut spans: Vec<SegmentRange> = Vec::with_capacity(segments.len());
    for segment in segments {
        let start = u32::try_from(wide.len()).unwrap_or(u32::MAX);
        wide.extend(segment.text.encode_utf16());
        let end = u32::try_from(wide.len()).unwrap_or(u32::MAX);
        spans.push(SegmentRange {
            start,
            end,
            underline: segment.underline,
        });
    }
    (wide, spans)
}

/// Empties the composition and closes it.
fn clear_and_end<Authority, BeforeEnd, AfterEnd>(
    ec: u32,
    composition: &ITfComposition,
    authority: &mut Authority,
    before_end: &mut BeforeEnd,
    after_end: &mut AfterEnd,
) -> Result<()>
where
    Authority: FnMut() -> Result<()>,
    BeforeEnd: FnMut(&ITfComposition) -> Result<()>,
    AfterEnd: FnMut(&ITfComposition) -> Result<()>,
{
    // SAFETY: the composition is live and `ec` is this session's write cookie.
    let range = checked_host_call(authority, || unsafe { composition.GetRange() })?;
    checked_host_call(authority, || {
        // SAFETY: this range came from the live composition under `ec`; an
        // empty replacement clears only that composition range.
        unsafe { range.SetText(ec, 0, &[]) }
    })?;
    end_composition(ec, composition, authority, before_end, after_end)
}

/// Runs the marker callbacks around precisely one host `EndComposition` call.
/// `after_end` runs after both success and failure so callers cannot leave an
/// expected-self-termination marker armed after the host returns an HRESULT.
fn end_composition<Authority, BeforeEnd, AfterEnd>(
    ec: u32,
    composition: &ITfComposition,
    authority: &mut Authority,
    before_end: &mut BeforeEnd,
    after_end: &mut AfterEnd,
) -> Result<()>
where
    Authority: FnMut() -> Result<()>,
    BeforeEnd: FnMut(&ITfComposition) -> Result<()>,
    AfterEnd: FnMut(&ITfComposition) -> Result<()>,
{
    // The authority check happens before arming the self-termination marker.
    // The marker therefore still covers exactly the host EndComposition call;
    // the post-call check runs only after `after_end` has cleared it.
    authority()?;
    let end_result = invoke_end_composition_callbacks(
        composition,
        before_end,
        |composition| {
            // SAFETY: `ec` is the write cookie for this session and the
            // composition was created under the same context.
            unsafe { composition.EndComposition(ec) }
        },
        after_end,
    );
    let authority_result = authority();
    end_result.and(authority_result)
}

fn invoke_end_composition_callbacks<T, BeforeEnd, End, AfterEnd>(
    composition: &T,
    before_end: &mut BeforeEnd,
    end: End,
    after_end: &mut AfterEnd,
) -> Result<()>
where
    BeforeEnd: FnMut(&T) -> Result<()>,
    End: FnOnce(&T) -> Result<()>,
    AfterEnd: FnMut(&T) -> Result<()>,
{
    before_end(composition)?;
    let end_result = end(composition);
    let after_result = after_end(composition);
    end_result.and(after_result)
}

/// Puts text straight into the document, with no composition around it.
fn insert_plain<Authority>(
    edit: &DocumentEdit,
    ec: u32,
    text: &str,
    authority: &mut Authority,
) -> Result<()>
where
    Authority: FnMut() -> Result<()>,
{
    let range = current_selection_range(&edit.context, ec, authority)?;
    let wide: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: `range` belongs to this context, `ec` is the active read/write
    // cookie, and `wide` remains alive for the duration of the call.
    checked_host_call(authority, || unsafe { range.SetText(ec, 0, &wide) })?;
    move_caret_to_end(edit, ec, &range, authority)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod end_composition_callback_tests {
    use std::cell::Cell;

    use windows::Win32::Foundation::E_UNEXPECTED;
    use windows_core::Error;

    use super::invoke_end_composition_callbacks;

    #[test]
    fn marker_is_active_only_while_the_end_callback_runs() {
        let marker = Cell::new(false);
        let before_end = Cell::new(false);
        let end_saw_marker = Cell::new(false);
        let after_end = Cell::new(false);
        let subject = ();

        let mut before = |_: &()| {
            assert!(
                !marker.get(),
                "marker must be inactive before EndComposition"
            );
            before_end.set(true);
            marker.set(true);
            Ok(())
        };
        let mut after = |_: &()| {
            assert!(
                marker.get(),
                "marker must stay armed through EndComposition"
            );
            after_end.set(true);
            marker.set(false);
            Ok(())
        };
        let result = invoke_end_composition_callbacks(
            &subject,
            &mut before,
            |_: &()| {
                end_saw_marker.set(marker.get());
                Ok(())
            },
            &mut after,
        );

        assert!(result.is_ok());
        assert!(before_end.get());
        assert!(end_saw_marker.get());
        assert!(after_end.get());
        assert!(!marker.get(), "marker must be cleared after EndComposition");
    }

    #[test]
    fn marker_is_cleared_when_end_composition_fails() {
        let marker = Cell::new(false);
        let after_end = Cell::new(false);
        let subject = ();

        let mut before = |_: &()| {
            marker.set(true);
            Ok(())
        };
        let mut after = |_: &()| {
            after_end.set(true);
            marker.set(false);
            Ok(())
        };
        let result = invoke_end_composition_callbacks(
            &subject,
            &mut before,
            |_: &()| Err(Error::from_hresult(E_UNEXPECTED)),
            &mut after,
        );

        assert!(result.is_err());
        assert!(after_end.get());
        assert!(!marker.get(), "marker must be cleared after an HRESULT");
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod authority_gate_tests {
    use std::cell::{Cell, RefCell};

    use windows::Win32::Foundation::E_UNEXPECTED;
    use windows_core::{Error, Result};

    use super::checked_host_call;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Operation {
        GetSelection,
        StartComposition,
        SetText,
        SetAttribute,
        SetSelection,
        EndComposition,
    }

    fn run_operation_trace<Authority, Execute>(
        operations: &[Operation],
        authority: &mut Authority,
        mut execute: Execute,
    ) -> Result<()>
    where
        Authority: FnMut() -> Result<()>,
        Execute: FnMut(Operation) -> Result<()>,
    {
        for operation in operations {
            checked_host_call(authority, || execute(*operation))?;
        }
        Ok(())
    }

    #[test]
    fn lifecycle_invalidation_after_one_host_call_blocks_every_later_operation() {
        let invalidated = Cell::new(false);
        let trace = RefCell::new(Vec::new());
        let mut authority = || {
            if invalidated.get() {
                Err(Error::from_hresult(E_UNEXPECTED))
            } else {
                Ok(())
            }
        };

        let result = run_operation_trace(
            &[
                Operation::GetSelection,
                Operation::StartComposition,
                Operation::SetText,
                Operation::SetAttribute,
                Operation::SetSelection,
                Operation::EndComposition,
            ],
            &mut authority,
            |operation| {
                trace.borrow_mut().push(operation);
                if operation == Operation::GetSelection {
                    // A fake host lifecycle callback invalidates the ticket
                    // while GetSelection is on the stack.
                    invalidated.set(true);
                }
                Ok(())
            },
        );

        assert!(result.is_err());
        assert_eq!(&*trace.borrow(), &[Operation::GetSelection]);
    }
}

#[cfg(test)]
mod candidate_geometry_authority_tests {
    use std::cell::{Cell, RefCell};

    use windows::Win32::Foundation::{E_UNEXPECTED, RECT};
    use windows_core::Error;

    use super::{query_candidate_rect_host_calls, GeometryResult};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum GeometryCall {
        Range,
        ActiveView,
        TextExt,
    }

    #[test]
    fn invalidation_during_get_range_skips_later_geometry_calls() {
        let invalidated = Cell::new(false);
        let trace = RefCell::new(Vec::new());
        let mut authority = || {
            if invalidated.get() {
                Err(Error::from_hresult(E_UNEXPECTED))
            } else {
                Ok(())
            }
        };

        let result = query_candidate_rect_host_calls(
            &mut authority,
            || {
                trace.borrow_mut().push(GeometryCall::Range);
                // Simulate a focus/context/lifecycle callback entered by
                // ITfComposition::GetRange.
                invalidated.set(true);
                Ok(())
            },
            || {
                trace.borrow_mut().push(GeometryCall::ActiveView);
                Ok(())
            },
            |_, _| {
                trace.borrow_mut().push(GeometryCall::TextExt);
                Ok(RECT::default())
            },
        );

        assert_eq!(result, GeometryResult::Unavailable);
        assert_eq!(&*trace.borrow(), &[GeometryCall::Range]);
    }

    #[test]
    fn invalidation_during_get_active_view_skips_text_ext() {
        let invalidated = Cell::new(false);
        let trace = RefCell::new(Vec::new());
        let mut authority = || {
            if invalidated.get() {
                Err(Error::from_hresult(E_UNEXPECTED))
            } else {
                Ok(())
            }
        };

        let result = query_candidate_rect_host_calls(
            &mut authority,
            || {
                trace.borrow_mut().push(GeometryCall::Range);
                Ok(())
            },
            || {
                trace.borrow_mut().push(GeometryCall::ActiveView);
                // Simulate an invalidating callback inside
                // ITfContext::GetActiveView.
                invalidated.set(true);
                Ok(())
            },
            |_, _| {
                trace.borrow_mut().push(GeometryCall::TextExt);
                Ok(RECT::default())
            },
        );

        assert_eq!(result, GeometryResult::Unavailable);
        assert_eq!(
            &*trace.borrow(),
            &[GeometryCall::Range, GeometryCall::ActiveView]
        );
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod commit_undo_delete_sequence_tests {
    use std::cell::{Cell, RefCell};

    use windows::Win32::Foundation::E_UNEXPECTED;
    use windows_core::Error;

    use super::{delete_before_host_calls, DeleteBeforeError};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum DeleteCall {
        IsEmpty,
        Collapse,
        ShiftStart,
        GetText,
        SetText,
    }

    fn invalidation_error() -> Error {
        Error::from_hresult(E_UNEXPECTED)
    }

    fn copy_fixture_text(actual: &mut [u16], text: &[u16]) {
        actual
            .get_mut(..text.len())
            .expect("host fixture buffer fits the supplied UTF-16 text")
            .copy_from_slice(text);
    }

    #[test]
    fn commit_undo_non_empty_selection_stops_before_any_range_mutation() {
        let expected: Vec<u16> = "行った".encode_utf16().collect();
        let trace = RefCell::new(Vec::new());
        let mut authority = || Ok(());

        let result = delete_before_host_calls(
            &expected,
            &mut authority,
            || {
                trace.borrow_mut().push(DeleteCall::IsEmpty);
                Ok(false)
            },
            || {
                trace.borrow_mut().push(DeleteCall::Collapse);
                Ok(())
            },
            |shifted| {
                trace.borrow_mut().push(DeleteCall::ShiftStart);
                *shifted = -(expected.len() as i32);
                Ok(())
            },
            |actual, copied| {
                trace.borrow_mut().push(DeleteCall::GetText);
                copy_fixture_text(actual, &expected);
                *copied = expected.len() as u32;
                Ok(())
            },
            || {
                trace.borrow_mut().push(DeleteCall::SetText);
                Ok(())
            },
        );

        assert!(matches!(result, Err(DeleteBeforeError::Validation(_))));
        assert_eq!(&*trace.borrow(), &[DeleteCall::IsEmpty]);
    }

    #[test]
    fn commit_undo_moved_caret_stops_before_text_read_or_delete() {
        let expected: Vec<u16> = "ab".encode_utf16().collect();
        let trace = RefCell::new(Vec::new());
        let mut authority = || Ok(());

        let result = delete_before_host_calls(
            &expected,
            &mut authority,
            || {
                trace.borrow_mut().push(DeleteCall::IsEmpty);
                Ok(true)
            },
            || {
                trace.borrow_mut().push(DeleteCall::Collapse);
                Ok(())
            },
            |shifted| {
                trace.borrow_mut().push(DeleteCall::ShiftStart);
                *shifted = -1;
                Ok(())
            },
            |_, _| {
                trace.borrow_mut().push(DeleteCall::GetText);
                Ok(())
            },
            || {
                trace.borrow_mut().push(DeleteCall::SetText);
                Ok(())
            },
        );

        assert!(matches!(result, Err(DeleteBeforeError::Validation(_))));
        assert_eq!(
            &*trace.borrow(),
            &[
                DeleteCall::IsEmpty,
                DeleteCall::Collapse,
                DeleteCall::ShiftStart,
            ]
        );
    }

    #[test]
    fn commit_undo_boundary_caret_stops_before_text_read_or_delete() {
        let expected: Vec<u16> = "ab".encode_utf16().collect();
        let trace = RefCell::new(Vec::new());
        let mut authority = || Ok(());

        let result = delete_before_host_calls(
            &expected,
            &mut authority,
            || {
                trace.borrow_mut().push(DeleteCall::IsEmpty);
                Ok(true)
            },
            || {
                trace.borrow_mut().push(DeleteCall::Collapse);
                Ok(())
            },
            |shifted| {
                trace.borrow_mut().push(DeleteCall::ShiftStart);
                *shifted = 0;
                Ok(())
            },
            |_, _| {
                trace.borrow_mut().push(DeleteCall::GetText);
                Ok(())
            },
            || {
                trace.borrow_mut().push(DeleteCall::SetText);
                Ok(())
            },
        );

        assert!(matches!(result, Err(DeleteBeforeError::Validation(_))));
        assert_eq!(
            &*trace.borrow(),
            &[
                DeleteCall::IsEmpty,
                DeleteCall::Collapse,
                DeleteCall::ShiftStart,
            ]
        );
    }

    #[test]
    fn commit_undo_same_utf16_length_different_text_stops_before_delete() {
        let expected: Vec<u16> = "ab".encode_utf16().collect();
        let actual_text: Vec<u16> = "ax".encode_utf16().collect();
        let set_calls = Cell::new(0);
        let mut authority = || Ok(());

        let result = delete_before_host_calls(
            &expected,
            &mut authority,
            || Ok(true),
            || Ok(()),
            |shifted| {
                *shifted = -(expected.len() as i32);
                Ok(())
            },
            |actual, copied| {
                copy_fixture_text(actual, &actual_text);
                *copied = actual_text.len() as u32;
                Ok(())
            },
            || {
                set_calls.set(set_calls.get() + 1);
                Ok(())
            },
        );

        assert!(matches!(result, Err(DeleteBeforeError::Validation(_))));
        assert_eq!(set_calls.get(), 0);
    }

    #[test]
    fn commit_undo_surrogate_pair_uses_utf16_units_and_sets_once() {
        let expected: Vec<u16> = "🍣".encode_utf16().collect();
        assert_eq!(expected.len(), 2);
        let set_calls = Cell::new(0);
        let mut authority = || Ok(());

        let result = delete_before_host_calls(
            &expected,
            &mut authority,
            || Ok(true),
            || Ok(()),
            |shifted| {
                *shifted = -2;
                Ok(())
            },
            |actual, copied| {
                copy_fixture_text(actual, &expected);
                *copied = expected.len() as u32;
                Ok(())
            },
            || {
                set_calls.set(set_calls.get() + 1);
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert_eq!(set_calls.get(), 1);
    }

    #[test]
    fn commit_undo_authority_invalid_before_set_rejects_without_delete() {
        let expected: Vec<u16> = "行った".encode_utf16().collect();
        let authority_calls = Cell::new(0);
        let set_calls = Cell::new(0);
        let mut authority = || {
            let call = authority_calls.get() + 1;
            authority_calls.set(call);
            if call == 9 {
                Err(invalidation_error())
            } else {
                Ok(())
            }
        };

        let result = delete_before_host_calls(
            &expected,
            &mut authority,
            || Ok(true),
            || Ok(()),
            |shifted| {
                *shifted = -(expected.len() as i32);
                Ok(())
            },
            |actual, copied| {
                copy_fixture_text(actual, &expected);
                *copied = expected.len() as u32;
                Ok(())
            },
            || {
                set_calls.set(set_calls.get() + 1);
                Ok(())
            },
        );

        assert!(matches!(result, Err(DeleteBeforeError::Validation(_))));
        assert_eq!(authority_calls.get(), 9);
        assert_eq!(set_calls.get(), 0);
    }

    #[test]
    fn commit_undo_authority_invalid_after_set_classifies_mutation_as_unknown() {
        let expected: Vec<u16> = "行った".encode_utf16().collect();
        let authority_calls = Cell::new(0);
        let set_calls = Cell::new(0);
        let mut authority = || {
            let call = authority_calls.get() + 1;
            authority_calls.set(call);
            if call == 10 {
                Err(invalidation_error())
            } else {
                Ok(())
            }
        };

        let result = delete_before_host_calls(
            &expected,
            &mut authority,
            || Ok(true),
            || Ok(()),
            |shifted| {
                *shifted = -(expected.len() as i32);
                Ok(())
            },
            |actual, copied| {
                copy_fixture_text(actual, &expected);
                *copied = expected.len() as u32;
                Ok(())
            },
            || {
                set_calls.set(set_calls.get() + 1);
                Ok(())
            },
        );

        assert!(matches!(result, Err(DeleteBeforeError::Mutation(_))));
        assert_eq!(authority_calls.get(), 10);
        assert_eq!(set_calls.get(), 1);
    }

    #[test]
    fn commit_undo_set_text_error_classifies_mutation_after_one_attempt() {
        let expected: Vec<u16> = "行った".encode_utf16().collect();
        let set_calls = Cell::new(0);
        let mut authority = || Ok(());

        let result = delete_before_host_calls(
            &expected,
            &mut authority,
            || Ok(true),
            || Ok(()),
            |shifted| {
                *shifted = -(expected.len() as i32);
                Ok(())
            },
            |actual, copied| {
                copy_fixture_text(actual, &expected);
                *copied = expected.len() as u32;
                Ok(())
            },
            || {
                set_calls.set(set_calls.get() + 1);
                Err(invalidation_error())
            },
        );

        assert!(matches!(result, Err(DeleteBeforeError::Mutation(_))));
        assert_eq!(set_calls.get(), 1);
    }

    #[test]
    fn commit_undo_exact_success_sets_verified_text_once() {
        let expected: Vec<u16> = "行った".encode_utf16().collect();
        let set_calls = Cell::new(0);
        let mut authority = || Ok(());

        let result = delete_before_host_calls(
            &expected,
            &mut authority,
            || Ok(true),
            || Ok(()),
            |shifted| {
                *shifted = -(expected.len() as i32);
                Ok(())
            },
            |actual, copied| {
                copy_fixture_text(actual, &expected);
                *copied = expected.len() as u32;
                Ok(())
            },
            || {
                set_calls.set(set_calls.get() + 1);
                Ok(())
            },
        );

        assert!(result.is_ok());
        assert_eq!(set_calls.get(), 1);
    }
}

#[cfg(test)]
mod compatibility_tests {
    #[test]
    fn electron_document_path_never_queries_insert_at_selection() {
        let source = include_str!("composition.rs");
        // Construct the name so this guard does not trigger on its own source.
        let forbidden = ["ITfInsert", "AtSelection"].concat();
        assert!(
            !source.contains(&forbidden),
            "Electron-compatible document edits must start from ITfContext::GetSelection"
        );
    }
}

/// Which published style a segment's underline kind draws with.
const fn display_attribute_guid(underline: UnderlineKind) -> GUID {
    match underline {
        UnderlineKind::Raw => GUID_DISPLAY_ATTRIBUTE_RAW,
        UnderlineKind::Converted => GUID_DISPLAY_ATTRIBUTE_CONVERTED,
        UnderlineKind::Focused => GUID_DISPLAY_ATTRIBUTE_FOCUSED,
    }
}

/// Marks each of `spans` with the display attribute for its underline kind,
/// slicing `range` -- the range [`write_text`] just filled -- into one
/// sub-range per span.
///
/// The text is already in the document by the time this runs, so a display
/// attribute failure must not cost the user their composition: a host that
/// refuses `RegisterGUID`/`SetValue` for one attribute, or a sub-range this
/// function could not compute, degrades that one segment to undecorated text
/// -- exactly what already happens when there is no category manager at all
/// -- instead of failing the whole write. Only the write's own authority
/// going stale is still fatal, because any call here re-entering a lifecycle
/// callback makes every later document operation on this ticket unsafe no
/// matter how the styling turned out.
fn apply_display_attributes<Authority>(
    edit: &DocumentEdit,
    ec: u32,
    range: &ITfRange,
    spans: &[SegmentRange],
    authority: &mut Authority,
) -> Result<()>
where
    Authority: FnMut() -> Result<()>,
{
    let Some(category_mgr) = edit.category_mgr.as_ref() else {
        return Ok(());
    };

    // SAFETY: the property GUID is a valid `'static` constant.
    let property = match checked_host_call(authority, || unsafe {
        edit.context.GetProperty(&GUID_PROP_ATTRIBUTE)
    }) {
        Ok(property) => property,
        // Getting the property is common to every segment, so losing it
        // degrades every segment to undecorated text rather than failing the
        // write -- but `authority()` still surfaces a ticket the failed call
        // may itself have invalidated.
        Err(_) => return authority(),
    };

    let total_len = spans.last().map_or(0, |span| span.end);
    for span in spans {
        let outcome = if spans.len() == 1 && span.start == 0 && span.end == total_len {
            // The common case: one segment covering everything that was just
            // written, so the range from `GetRange` needs no slicing at all.
            apply_one_attribute(
                category_mgr,
                &property,
                ec,
                range,
                span.underline,
                authority,
            )
        } else {
            segment_sub_range(ec, range, span.start, span.end, total_len, authority).and_then(
                |sub| {
                    apply_one_attribute(
                        category_mgr,
                        &property,
                        ec,
                        &sub,
                        span.underline,
                        authority,
                    )
                },
            )
        };
        if outcome.is_err() {
            // Degrade this one segment's styling and move on, unless the
            // failure was really the ticket going stale underneath us.
            authority()?;
        }
    }
    Ok(())
}

/// Clones `range` and narrows the clone to the UTF-16 sub-span `[start, end)`
/// measured from `range`'s own start; `total_len` is `range`'s current
/// length, i.e. the distance the clone's end has to be pulled back before its
/// start can be pushed forward.
///
/// Returns an error rather than a range that might cover the wrong span if a
/// shift did not move by exactly the amount requested: this feeds directly
/// into which text gets which underline, so a silently short shift would
/// style the wrong segment instead of just failing to style one.
fn segment_sub_range<Authority>(
    ec: u32,
    range: &ITfRange,
    start: u32,
    end: u32,
    total_len: u32,
    authority: &mut Authority,
) -> Result<ITfRange>
where
    Authority: FnMut() -> Result<()>,
{
    // SAFETY: `range` is the live range `write_text` just filled.
    let sub = checked_host_call(authority, || unsafe { range.Clone() })?;

    let end_back = i32::try_from(total_len.saturating_sub(end)).unwrap_or(i32::MAX);
    let mut end_shifted = 0i32;
    // SAFETY: `sub` is this call's own fresh clone of `range`.
    checked_host_call(authority, || unsafe {
        sub.ShiftEnd(ec, -end_back, &mut end_shifted, core::ptr::null())
    })?;
    if end_shifted != -end_back {
        return Err(Error::new(
            E_UNEXPECTED,
            "display attribute range did not shift to the requested segment end",
        ));
    }

    let start_forward = i32::try_from(start).unwrap_or(i32::MAX);
    let mut start_shifted = 0i32;
    // SAFETY: as above.
    checked_host_call(authority, || unsafe {
        sub.ShiftStart(ec, start_forward, &mut start_shifted, core::ptr::null())
    })?;
    if start_shifted != start_forward {
        return Err(Error::new(
            E_UNEXPECTED,
            "display attribute range did not shift to the requested segment start",
        ));
    }

    Ok(sub)
}

/// Registers and applies the display attribute for `underline` over `range`.
fn apply_one_attribute<Authority>(
    category_mgr: &ITfCategoryMgr,
    property: &ITfProperty,
    ec: u32,
    range: &ITfRange,
    underline: UnderlineKind,
    authority: &mut Authority,
) -> Result<()>
where
    Authority: FnMut() -> Result<()>,
{
    let guid = display_attribute_guid(underline);
    // SAFETY: the GUID is a valid `'static` constant.
    let atom = checked_host_call(authority, || unsafe { category_mgr.RegisterGUID(&guid) })?;

    // The atom is an opaque token, so reinterpreting its bits as the `VT_I4`
    // this property expects loses nothing — TSF hands the same bits back.
    let value = variant::from_i4(atom as i32);
    // SAFETY: `value` outlives the call and holds no owned resource, and
    // `range` is a live range under `ec`.
    checked_host_call(authority, || unsafe {
        property.SetValue(ec, range, &value)
    })
}

#[cfg(test)]
mod display_attribute_tests {
    use super::{
        display_attribute_guid, segment_ranges, segments_have_text, Segment, SegmentRange,
        UnderlineKind,
    };
    use sakura_reg::{
        GUID_DISPLAY_ATTRIBUTE_CONVERTED, GUID_DISPLAY_ATTRIBUTE_FOCUSED,
        GUID_DISPLAY_ATTRIBUTE_RAW,
    };

    fn segment(text: &str, underline: UnderlineKind) -> Segment {
        Segment {
            text: text.to_owned(),
            underline,
        }
    }

    #[test]
    fn each_underline_kind_maps_to_its_own_published_guid() {
        assert_eq!(
            display_attribute_guid(UnderlineKind::Raw),
            GUID_DISPLAY_ATTRIBUTE_RAW
        );
        assert_eq!(
            display_attribute_guid(UnderlineKind::Converted),
            GUID_DISPLAY_ATTRIBUTE_CONVERTED
        );
        assert_eq!(
            display_attribute_guid(UnderlineKind::Focused),
            GUID_DISPLAY_ATTRIBUTE_FOCUSED
        );
    }

    /// A single raw segment is the overwhelming common case (plain kana
    /// input with no conversion yet), so it has to land on exactly the whole
    /// written range -- the condition `apply_display_attributes` uses to
    /// skip cloning a sub-range at all -- and still resolve to the RAW GUID.
    #[test]
    fn single_segment_raw_preedit_spans_the_whole_written_range_and_maps_to_raw_guid() {
        let segments = [segment("たべる", UnderlineKind::Raw)];
        let (wide, spans) = segment_ranges(&segments);

        assert_eq!(wide.len(), 3, "「たべる」 is three UTF-16 code units");
        assert_eq!(
            spans,
            [SegmentRange {
                start: 0,
                end: 3,
                underline: UnderlineKind::Raw,
            }]
        );
        let only = spans.first().copied().unwrap_or(SegmentRange {
            start: u32::MAX,
            end: u32::MAX,
            underline: UnderlineKind::Raw,
        });
        assert!(spans.len() == 1 && only.start == 0 && only.end as usize == wide.len());
        assert_eq!(
            display_attribute_guid(only.underline),
            GUID_DISPLAY_ATTRIBUTE_RAW
        );
    }

    /// A converted, multi-bunsetsu preedit: each segment's UTF-16 span has to
    /// land exactly on its own text and nowhere else, and each underline kind
    /// has to resolve to a different GUID -- otherwise the host would draw
    /// the focused clause identically to its unconverted neighbour.
    #[test]
    fn multi_segment_preedit_maps_each_segment_to_its_own_utf16_span_and_guid() {
        // "わたし" (Converted, 3 code units) + "は" (Focused, 1 code unit) +
        // "にほんじん" (Raw, 5 code units) -- three bunsetsu with three
        // different underline kinds, none of them ASCII.
        let segments = [
            segment("わたし", UnderlineKind::Converted),
            segment("は", UnderlineKind::Focused),
            segment("にほんじん", UnderlineKind::Raw),
        ];
        let (wide, spans) = segment_ranges(&segments);

        assert_eq!(wide.len(), 9);
        assert_eq!(
            spans,
            [
                SegmentRange {
                    start: 0,
                    end: 3,
                    underline: UnderlineKind::Converted,
                },
                SegmentRange {
                    start: 3,
                    end: 4,
                    underline: UnderlineKind::Focused,
                },
                SegmentRange {
                    start: 4,
                    end: 9,
                    underline: UnderlineKind::Raw,
                },
            ]
        );
        let actual_guids = spans
            .iter()
            .map(|span| display_attribute_guid(span.underline))
            .collect::<Vec<_>>();
        assert_eq!(
            actual_guids,
            [
                GUID_DISPLAY_ATTRIBUTE_CONVERTED,
                GUID_DISPLAY_ATTRIBUTE_FOCUSED,
                GUID_DISPLAY_ATTRIBUTE_RAW,
            ]
        );
    }

    /// A surrogate-pair character (outside the Basic Multilingual Plane)
    /// takes two UTF-16 code units, not one -- the same distinction the
    /// commit-undo tests in this file already guard for `ShiftStart`. A
    /// segment boundary that used chars or UTF-8 bytes instead of UTF-16
    /// units here would corrupt every later segment's range.
    #[test]
    fn a_surrogate_pair_segment_advances_the_offset_by_two_utf16_units() {
        let segments = [
            segment("🍣", UnderlineKind::Raw),
            segment("たべた", UnderlineKind::Converted),
        ];
        let (wide, spans) = segment_ranges(&segments);

        assert_eq!(
            wide.len(),
            5,
            "one surrogate pair plus three BMP characters"
        );
        assert_eq!(
            spans,
            [
                SegmentRange {
                    start: 0,
                    end: 2,
                    underline: UnderlineKind::Raw,
                },
                SegmentRange {
                    start: 2,
                    end: 5,
                    underline: UnderlineKind::Converted,
                },
            ]
        );
    }

    #[test]
    fn segments_have_text_is_false_for_no_segments_or_all_empty_segments() {
        assert!(!segments_have_text(&[]));
        assert!(!segments_have_text(&[segment("", UnderlineKind::Raw)]));
        assert!(segments_have_text(&[
            segment("", UnderlineKind::Raw),
            segment("か", UnderlineKind::Raw),
        ]));
    }
}

/// Collapses the caret to just after `range`.
fn move_caret_to_end<Authority>(
    edit: &DocumentEdit,
    ec: u32,
    range: &ITfRange,
    authority: &mut Authority,
) -> Result<()>
where
    Authority: FnMut() -> Result<()>,
{
    // A clone, because collapsing the composition's own range would shrink the
    // composition to nothing.
    // SAFETY: `range` is live and `ec` is this session's write cookie.
    let caret = checked_host_call(authority, || unsafe { range.Clone() })?;
    // SAFETY: as above.
    checked_host_call(authority, || unsafe { caret.Collapse(ec, TF_ANCHOR_END) })?;
    select_range(&edit.context, ec, caret, authority)
}

/// Moves the document's selection to `range`.
///
/// `TF_SELECTION` holds its range in a `ManuallyDrop`, so the reference count
/// this function is holding has to be released by hand once TSF has read it.
pub(crate) fn select_range<Authority>(
    context: &ITfContext,
    ec: u32,
    range: ITfRange,
    authority: &mut Authority,
) -> Result<()>
where
    Authority: FnMut() -> Result<()>,
{
    let mut selection = TF_SELECTION {
        range: ManuallyDrop::new(Some(range)),
        style: TF_SELECTIONSTYLE {
            ase: TF_AE_NONE,
            fInterimChar: false.into(),
        },
    };

    // SAFETY: the slice describes exactly one initialized selection and TSF only
    // reads it.
    let result = checked_host_call(authority, || unsafe {
        context.SetSelection(ec, core::slice::from_ref(&selection))
    });

    // SAFETY: this function created the `ManuallyDrop` and `SetSelection`
    // borrows rather than takes it, so this is the one and only release.
    unsafe { ManuallyDrop::drop(&mut selection.range) };
    result
}

/// The one `VARIANT` shape this crate needs.
mod variant {
    use core::mem::ManuallyDrop;
    use windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_I4};

    /// Builds a `VT_I4` variant.
    ///
    /// `VT_I4` owns nothing, so the result needs no `VariantClear` and can be
    /// dropped like any other value — which is the only reason hand-building a
    /// `VARIANT` is safe to do here.
    pub fn from_i4(value: i32) -> VARIANT {
        VARIANT {
            Anonymous: VARIANT_0 {
                Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                    vt: VT_I4,
                    wReserved1: 0,
                    wReserved2: 0,
                    wReserved3: 0,
                    Anonymous: VARIANT_0_0_0 { lVal: value },
                }),
            },
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn carries_the_value_it_was_given() {
            let variant = from_i4(0x1234_5678);
            // SAFETY: `from_i4` just wrote exactly these union arms.
            unsafe {
                assert_eq!(variant.Anonymous.Anonymous.vt, VT_I4);
                assert_eq!(variant.Anonymous.Anonymous.Anonymous.lVal, 0x1234_5678);
            }
        }
    }
}
