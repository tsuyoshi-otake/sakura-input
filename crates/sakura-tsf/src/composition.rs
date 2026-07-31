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

use windows::Win32::UI::TextServices::{
    ITfCategoryMgr, ITfComposition, ITfCompositionSink, ITfContext, ITfContextComposition,
    ITfInsertAtSelection, ITfRange, GUID_PROP_ATTRIBUTE, TF_AE_NONE, TF_ANCHOR_END, TF_IAS_NOQUERY,
    TF_IAS_QUERYONLY, TF_SELECTION, TF_SELECTIONSTYLE,
};
use windows_core::{Interface, Result};

use sakura_reg::GUID_DISPLAY_ATTRIBUTE_RAW;

/// What the document should be made to show.
///
/// The text travels by value because the edit that applies it may run long after
/// the keystroke that produced it, by which time the live preedit has moved on.
#[derive(Clone, Debug)]
pub enum Update {
    /// Show this as an active, underlined preedit.
    Show(String),
    /// Show this and then hand it to the application as ordinary text.
    Commit(String),
    /// Throw the preedit away without committing anything.
    Discard,
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

/// Makes the document match `update`.
///
/// `handle` is updated in place rather than returned so that a failure partway
/// through still leaves the caller holding whatever composition really exists.
/// Dropping a live `ITfComposition` on the error path would strand an underlined
/// run in the user's document that nothing can ever end.
pub fn apply(
    edit: &DocumentEdit,
    ec: u32,
    handle: &mut Option<ITfComposition>,
    update: &Update,
) -> Result<()> {
    match update {
        Update::Show(text) if !text.is_empty() => {
            if handle.is_none() {
                *handle = Some(start(edit, ec)?);
            }
            let Some(composition) = handle.as_ref() else {
                return Ok(());
            };
            write_text(edit, ec, composition, text)
        }

        // An empty preedit is not something the user can see, and a zero-width
        // composition left in the document swallows the caret in some hosts.
        Update::Show(_) | Update::Discard => match handle.take() {
            Some(composition) => clear_and_end(ec, &composition),
            None => Ok(()),
        },

        Update::Commit(text) => match handle.take() {
            Some(composition) => {
                if text.is_empty() {
                    return clear_and_end(ec, &composition);
                }
                write_text(edit, ec, &composition, text)?;
                // SAFETY: `ec` is the write cookie for this session and the
                // composition was created under the same context.
                unsafe { composition.EndComposition(ec) }
            }
            // Committing without an open composition is not an error: the host
            // may have terminated ours (OnCompositionTerminated) between the
            // keystroke and this lock. The text still belongs in the document.
            None if !text.is_empty() => insert_plain(edit, ec, text),
            None => Ok(()),
        },
    }
}

/// Opens a composition at the caret.
fn start(edit: &DocumentEdit, ec: u32) -> Result<ITfComposition> {
    let insert: ITfInsertAtSelection = edit.context.cast()?;
    // Inserting nothing is how TSF hands out a range at the caret: `QUERYONLY`
    // asks where the text *would* go without putting anything there.
    // SAFETY: `ec` is this session's write cookie; the empty slice is valid.
    let range = unsafe { insert.InsertTextAtSelection(ec, TF_IAS_QUERYONLY, &[])? };

    let composition: ITfContextComposition = edit.context.cast()?;
    // SAFETY: `range` was just produced by this context and `sink` outlives the
    // composition — it is the text service itself.
    unsafe { composition.StartComposition(ec, &range, &edit.sink) }
}

/// Replaces the composition's contents and puts the caret after them.
fn write_text(
    edit: &DocumentEdit,
    ec: u32,
    composition: &ITfComposition,
    text: &str,
) -> Result<()> {
    // SAFETY: the composition is live and `ec` is this session's write cookie.
    let range = unsafe { composition.GetRange()? };

    // UTF-16 at the TSF boundary (DESIGN 5.2). A `Vec` rather than a stack
    // buffer because a preedit has no fixed maximum length.
    let wide: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: `wide` outlives the call and its length is passed with it.
    unsafe { range.SetText(ec, 0, &wide)? };

    apply_display_attribute(edit, ec, &range)?;
    move_caret_to_end(edit, ec, &range)
}

/// Empties the composition and closes it.
fn clear_and_end(ec: u32, composition: &ITfComposition) -> Result<()> {
    // SAFETY: the composition is live and `ec` is this session's write cookie.
    unsafe {
        let range = composition.GetRange()?;
        range.SetText(ec, 0, &[])?;
        composition.EndComposition(ec)
    }
}

/// Puts text straight into the document, with no composition around it.
fn insert_plain(edit: &DocumentEdit, ec: u32, text: &str) -> Result<()> {
    let insert: ITfInsertAtSelection = edit.context.cast()?;
    let wide: Vec<u16> = text.encode_utf16().collect();
    // SAFETY: `wide` outlives the call; `ec` is this session's write cookie.
    let range = unsafe { insert.InsertTextAtSelection(ec, TF_IAS_NOQUERY, &wide)? };
    move_caret_to_end(edit, ec, &range)
}

/// Marks `range` with the "unconverted input" style.
///
/// Milestone 3 has no conversion, so every character is raw input; the other two
/// published styles come into use once the engine returns segmented output.
fn apply_display_attribute(edit: &DocumentEdit, ec: u32, range: &ITfRange) -> Result<()> {
    let Some(category_mgr) = edit.category_mgr.as_ref() else {
        return Ok(());
    };

    // SAFETY: the GUID is a valid `'static` constant.
    let atom = unsafe { category_mgr.RegisterGUID(&GUID_DISPLAY_ATTRIBUTE_RAW)? };
    // SAFETY: the property GUID is a valid `'static` constant.
    let property = unsafe { edit.context.GetProperty(&GUID_PROP_ATTRIBUTE)? };

    // The atom is an opaque token, so reinterpreting its bits as the `VT_I4`
    // this property expects loses nothing — TSF hands the same bits back.
    let value = variant::from_i4(atom as i32);
    // SAFETY: `value` outlives the call and holds no owned resource.
    unsafe { property.SetValue(ec, range, &value) }
}

/// Collapses the caret to just after `range`.
fn move_caret_to_end(edit: &DocumentEdit, ec: u32, range: &ITfRange) -> Result<()> {
    // A clone, because collapsing the composition's own range would shrink the
    // composition to nothing.
    // SAFETY: `range` is live and `ec` is this session's write cookie.
    let caret = unsafe { range.Clone()? };
    // SAFETY: as above.
    unsafe { caret.Collapse(ec, TF_ANCHOR_END)? };
    set_selection(&edit.context, ec, caret)
}

/// Moves the document's selection to `range`.
///
/// `TF_SELECTION` holds its range in a `ManuallyDrop`, so the reference count
/// this function is holding has to be released by hand once TSF has read it.
fn set_selection(context: &ITfContext, ec: u32, range: ITfRange) -> Result<()> {
    let mut selection = TF_SELECTION {
        range: ManuallyDrop::new(Some(range)),
        style: TF_SELECTIONSTYLE {
            ase: TF_AE_NONE,
            fInterimChar: false.into(),
        },
    };

    // SAFETY: the slice describes exactly one initialized selection and TSF only
    // reads it.
    let result = unsafe { context.SetSelection(ec, core::slice::from_ref(&selection)) };

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
