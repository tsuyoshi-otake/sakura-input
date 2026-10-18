//! The Pad's clipboard writes.
//!
//! Everything the Pad puts on the clipboard goes through this module so the
//! privacy marks that decide whether Windows clipboard history (Win+V) and
//! the cloud clipboard may keep a copy live in exactly one place. A copy of
//! an unlocked protected memo is [`ClipboardPrivacy::Protected`]; every other
//! copy is [`ClipboardPrivacy::Normal`].

use std::mem::size_of;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use zeroize::Zeroizing;

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
#[derive(Debug, Clone, Copy)]
pub(crate) struct ClipboardMarker {
    pub(crate) name: &'static str,
    pub(crate) value: u32,
}

/// Registered clipboard formats documented to exclude a clipboard session
/// from history, cloud sync, and clipboard-monitor processing. A present
/// format marks the whole session; the DWORD payloads are the documented
/// values for "exclude" (1) and "do not keep" (0).
const PROTECTED_MARKERS: &[ClipboardMarker] = &[
    ClipboardMarker {
        name: "ExcludeClipboardContentFromMonitorProcessing",
        value: 1,
    },
    ClipboardMarker {
        name: "CanIncludeInClipboardHistory",
        value: 0,
    },
    ClipboardMarker {
        name: "CanUploadToCloudClipboard",
        value: 0,
    },
];

impl ClipboardPrivacy {
    /// The markers placed alongside the text in the same clipboard session.
    fn markers(self) -> &'static [ClipboardMarker] {
        match self {
            Self::Normal => &[],
            Self::Protected => PROTECTED_MARKERS,
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

fn place_text(owner: HWND, value: &str, markers: &[ClipboardMarker]) -> bool {
    // One allocation up front: a UTF-16 string never has more code units
    // than the UTF-8 source has bytes, so the buffer never reallocates and
    // no unzeroized copy of the secret is left on the freed heap.
    let mut wide = Zeroizing::new(Vec::with_capacity(value.len() + 1));
    let wide_capacity = wide.capacity();
    wide.extend(value.encode_utf16());
    wide.push(0);
    debug_assert_eq!(wide.capacity(), wide_capacity);
    let bytes = wide.len().saturating_mul(size_of::<u16>());
    // SAFETY: every handle is checked, the copy is bounded by the allocation
    // requested for it, the clipboard is closed on every path, and `wide` is
    // zeroized on drop on every path.
    unsafe {
        let Ok(text) = GlobalAlloc(GMEM_MOVEABLE, bytes) else {
            return false;
        };
        let target = GlobalLock(text);
        if target.is_null() {
            let _ = free_global(text);
            return false;
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), target.cast::<u16>(), wide.len());
        let _ = GlobalUnlock(text);
        if OpenClipboard(Some(owner)).is_err() {
            let _ = free_global(text);
            return false;
        }
        if EmptyClipboard().is_err() {
            let _ = CloseClipboard();
            let _ = free_global(text);
            return false;
        }
        // The markers go on before the text so a marker failure can never
        // leave the secret behind: the failure path empties the clipboard
        // (which frees every block `SetClipboardData` already accepted) and
        // frees the text block this call still owns. A block the clipboard
        // accepted is never freed here.
        for marker in markers {
            let mut name: Vec<u16> = marker.name.encode_utf16().collect();
            name.push(0);
            let format = RegisterClipboardFormatW(PCWSTR(name.as_ptr()));
            if format == 0 {
                return fail_closed(text);
            }
            let Ok(block) = GlobalAlloc(GMEM_MOVEABLE, size_of::<u32>()) else {
                return fail_closed(text);
            };
            let slot = GlobalLock(block);
            if slot.is_null() {
                let _ = free_global(block);
                return fail_closed(text);
            }
            *(slot.cast::<u32>()) = marker.value;
            let _ = GlobalUnlock(block);
            if SetClipboardData(format, Some(HANDLE(block.0))).is_err() {
                let _ = free_global(block);
                return fail_closed(text);
            }
        }
        let format = windows::Win32::System::Ole::CF_UNICODETEXT.0 as u32;
        if SetClipboardData(format, Some(HANDLE(text.0))).is_ok() {
            let _ = CloseClipboard();
            return true;
        }
        fail_closed(text)
    }
}

/// Fails a `place_text` call with the clipboard open: empties the session so
/// nothing partial survives, frees the unplaced text block, and reports the
/// failure. Only called while `text` has not been handed to the clipboard.
unsafe fn fail_closed(text: HGLOBAL) -> bool {
    // SAFETY: the caller holds the clipboard open; `EmptyClipboard` frees the
    // marker blocks the clipboard accepted, and `text` was never accepted.
    unsafe {
        let _ = EmptyClipboard();
        let _ = CloseClipboard();
        let _ = free_global(text);
    }
    false
}

fn free_global(block: HGLOBAL) -> bool {
    // SAFETY: the block is owned by this call chain and not on the clipboard.
    unsafe { GlobalFree(Some(block)).is_ok() }
}

#[cfg(test)]
#[path = "pad_clipboard_tests.rs"]
mod tests;
