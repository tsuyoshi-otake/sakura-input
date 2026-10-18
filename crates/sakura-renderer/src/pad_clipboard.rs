//! The Pad's clipboard writes.
//!
//! Everything the Pad puts on the clipboard goes through this module so the
//! privacy marks that decide whether Windows clipboard history (Win+V) and
//! the cloud clipboard may keep a copy live in exactly one place. A copy of
//! an unlocked protected memo is [`ClipboardPrivacy::Protected`]; every other
//! copy is [`ClipboardPrivacy::Normal`].

use std::mem::size_of;

use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

/// How much trust Windows may extend to a copy beyond the current paste.
///
/// `Protected` copies carry the registered formats that keep the content out
/// of clipboard history and the cloud clipboard; `Normal` copies place only
/// the text, as they always have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClipboardPrivacy {
    Normal,
    Protected,
}

/// One registered clipboard format name and the DWORD payload that marks the
/// whole clipboard session when the format is present.
// The fields are consumed by `place_text` once the protected markers land;
// the plumbing arrives ahead of the behavior change.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub(crate) struct ClipboardMarker {
    pub(crate) name: &'static str,
    pub(crate) value: u32,
}

impl ClipboardPrivacy {
    /// The markers placed alongside the text in the same clipboard session.
    ///
    /// Both variants return an empty slice until the protected markers are
    /// wired up; `place_text` accepts the list so the plumbing is reviewed
    /// separately from the behavior change.
    fn markers(self) -> &'static [ClipboardMarker] {
        match self {
            Self::Normal => &[],
            Self::Protected => &[],
        }
    }
}

/// Puts `value` on the clipboard as Unicode text.
///
/// The block is freed on every failing path and only on those: a successful
/// `SetClipboardData` transfers ownership to the clipboard, and freeing it
/// afterwards is a use-after-free the next paste would find.
pub(crate) fn copy_to_clipboard(owner: HWND, value: &str, privacy: ClipboardPrivacy) -> bool {
    place_text(owner, value, privacy.markers())
}

fn place_text(owner: HWND, value: &str, _markers: &[ClipboardMarker]) -> bool {
    let mut wide: Vec<u16> = value.encode_utf16().collect();
    wide.push(0);
    let bytes = wide.len().saturating_mul(size_of::<u16>());
    // SAFETY: every handle is checked, the copy is bounded by the allocation
    // requested for it, and the clipboard is closed on every path.
    unsafe {
        let Ok(block) = GlobalAlloc(GMEM_MOVEABLE, bytes) else {
            return false;
        };
        let target = GlobalLock(block);
        if target.is_null() {
            let _ = free_global(block);
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), target.cast::<u16>(), wide.len());
        let _ = GlobalUnlock(block);
        if OpenClipboard(Some(owner)).is_err() {
            let _ = free_global(block);
            return false;
        }
        if EmptyClipboard().is_err() {
            let _ = CloseClipboard();
            let _ = free_global(block);
            return false;
        }
        let format = windows::Win32::System::Ole::CF_UNICODETEXT.0 as u32;
        let placed = SetClipboardData(format, Some(HANDLE(block.0))).is_ok();
        let _ = CloseClipboard();
        if !placed {
            let _ = free_global(block);
        }
        placed
    }
}

fn free_global(block: HGLOBAL) -> bool {
    // SAFETY: the block is owned by this call chain and not on the clipboard.
    unsafe { GlobalFree(Some(block)).is_ok() }
}

#[cfg(test)]
#[path = "pad_clipboard_tests.rs"]
mod tests;
