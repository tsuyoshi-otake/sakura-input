//! Owned process and token handles shared by client admission and server
//! trust, plus the SID and UTF-16 helpers both sides use.

use std::ffi::{c_void, OsString};
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::core::{Error, Result, HRESULT, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_DATA, HANDLE, HLOCAL,
    WIN32_ERROR,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenGroups,
    TokenIntegrityLevel, TokenUser, PSID, TOKEN_GROUPS, TOKEN_INFORMATION_CLASS,
    TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// `SE_GROUP_LOGON_ID` from `winnt.h`: marks the one group SID in a token
/// that identifies the logon session.
///
/// Spelled out here rather than taken from the bindings because the exact
/// path and type of this constant has moved between `windows` releases, and
/// a numeric constant from a header is not going to change.
const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;

/// A process handle opened only for the two read-only identity queries used
/// by this module.
pub(super) struct ProcessHandle {
    pub(super) handle: HANDLE,
}

impl ProcessHandle {
    pub(super) fn open(process_id: u32) -> Result<Self> {
        if process_id == 0 {
            return Err(Error::from_hresult(HRESULT::from_win32(
                ERROR_INVALID_DATA.0,
            )));
        }
        // SAFETY: the access mask is read-only and the PID came from a kernel
        // pipe query or a caller that is being classified. The returned
        // handle is owned by this wrapper and closed on drop.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id)? };
        Ok(Self { handle })
    }

    pub(super) fn image_path(&self, format: PROCESS_NAME_FORMAT) -> Result<PathBuf> {
        // Windows documents 32,767 UTF-16 code units as the maximum extended
        // path. A full fixed buffer avoids a retry race while keeping this
        // one-time admission query bounded.
        let mut buffer = vec![0u16; 32_768];
        let mut length = buffer.len() as u32;
        // SAFETY: the buffer is writable and its capacity is reported in
        // UTF-16 code units exactly as QueryFullProcessImageNameW requires.
        unsafe {
            QueryFullProcessImageNameW(
                self.handle,
                format,
                PWSTR(buffer.as_mut_ptr()),
                &mut length,
            )?;
        }
        if length == 0 || length as usize > buffer.len() {
            return Err(Error::from_hresult(HRESULT::from_win32(
                ERROR_INVALID_DATA.0,
            )));
        }
        buffer.truncate(length as usize);
        Ok(PathBuf::from(OsString::from_wide(&buffer)))
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: this handle came from OpenProcess and is closed once.
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

/// The current process's access token, closed on drop.
pub(super) struct ProcessToken {
    pub(super) handle: HANDLE,
}

impl ProcessToken {
    pub(super) fn open() -> Result<Self> {
        let mut handle = HANDLE::default();
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
        // release, and `handle` is a valid out-parameter.
        unsafe {
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle)?;
        }
        Ok(ProcessToken { handle })
    }

    pub(super) fn open_process(process: &ProcessHandle) -> Result<Self> {
        let mut handle = HANDLE::default();
        // SAFETY: `process.handle` is live and only queried for its token;
        // `handle` is a valid output location owned by this wrapper.
        unsafe {
            OpenProcessToken(process.handle, TOKEN_QUERY, &mut handle)?;
        }
        Ok(ProcessToken { handle })
    }

    /// The token's user SID, as a string.
    pub(super) fn user_sid(&self) -> Result<String> {
        let buf = self.information(TokenUser)?;
        // SAFETY: `information` filled `buf` with a `TOKEN_USER` for this
        // class, and the buffer is `u64`-aligned so the struct's pointer
        // field is correctly aligned.
        let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        sid_to_string(sid)
    }

    /// The token's logon-session SID, if it has one.
    pub(super) fn logon_sid(&self) -> Result<Option<String>> {
        let buf = self.information(TokenGroups)?;
        // SAFETY: as above, for `TOKEN_GROUPS`.
        let groups = unsafe { &*buf.as_ptr().cast::<TOKEN_GROUPS>() };
        // SAFETY: `TOKEN_GROUPS` declares `Groups` as a one-element array
        // standing in for a `GroupCount`-element one; the API contract is
        // that `GroupCount` entries follow, all within the buffer we sized
        // from the same call.
        let entries = unsafe {
            core::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize)
        };
        for entry in entries {
            if entry.Attributes & SE_GROUP_LOGON_ID != 0 {
                return sid_to_string(entry.Sid).map(Some);
            }
        }
        Ok(None)
    }

    /// Queries one class of token information into a `u64`-aligned buffer.
    ///
    /// The alignment is the point of returning `Vec<u64>` rather than the
    /// obvious `Vec<u8>`: every one of these structures starts with a
    /// pointer, and reading a pointer out of a byte buffer that happens to
    /// be misaligned is undefined behaviour that would work anyway on x86
    /// and stop working on ARM64.
    fn information(&self, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<u64>> {
        let mut needed = 0u32;
        // SAFETY: a null buffer with zero length is the documented way to
        // ask for the required size; `needed` is a valid out-parameter.
        let probe = unsafe { GetTokenInformation(self.handle, class, None, 0, &mut needed) };
        if let Err(error) = probe {
            if error.code() != win32_hresult(ERROR_INSUFFICIENT_BUFFER) {
                return Err(error);
            }
        }
        if needed == 0 {
            return Err(Error::from_hresult(win32_hresult(
                ERROR_INSUFFICIENT_BUFFER,
            )));
        }
        let words = (needed as usize).div_ceil(core::mem::size_of::<u64>());
        let mut buf = vec![0u64; words];
        let capacity = (words * core::mem::size_of::<u64>()) as u32;
        // SAFETY: `buf` holds at least `needed` bytes and is `u64`-aligned;
        // the pointer is valid for the duration of the call.
        unsafe {
            GetTokenInformation(
                self.handle,
                class,
                Some(buf.as_mut_ptr().cast::<c_void>()),
                capacity,
                &mut needed,
            )?;
        }
        Ok(buf)
    }
}

impl ProcessToken {
    pub(super) fn scalar(&self, class: TOKEN_INFORMATION_CLASS) -> Result<u32> {
        let buf = self.information(class)?;
        if buf.len() * core::mem::size_of::<u64>() < core::mem::size_of::<u32>() {
            return Err(Error::from_hresult(HRESULT::from_win32(
                ERROR_INVALID_DATA.0,
            )));
        }
        // SAFETY: `information` returns a u64-aligned allocation containing
        // the scalar requested by this token-information class.
        Ok(unsafe { *buf.as_ptr().cast::<u32>() })
    }

    pub(super) fn integrity_rid(&self) -> Result<u32> {
        let buf = self.information(TokenIntegrityLevel)?;
        if buf.len() * core::mem::size_of::<u64>() < core::mem::size_of::<TOKEN_MANDATORY_LABEL>() {
            return Err(Error::from_hresult(HRESULT::from_win32(
                ERROR_INVALID_DATA.0,
            )));
        }
        // SAFETY: the buffer is u64-aligned and was filled for
        // TokenIntegrityLevel, whose leading value is TOKEN_MANDATORY_LABEL.
        let label = unsafe { &*buf.as_ptr().cast::<TOKEN_MANDATORY_LABEL>() };
        if label.Label.Sid.is_invalid() {
            return Err(Error::from_hresult(HRESULT::from_win32(
                ERROR_INVALID_DATA.0,
            )));
        }
        // SAFETY: the SID pointer and its count are supplied by the same
        // kernel token-information buffer and remain live for this scope.
        let count = unsafe { GetSidSubAuthorityCount(label.Label.Sid) };
        // SAFETY: a non-null `count` points into the live token-information
        // buffer returned above.
        if count.is_null() || unsafe { *count } == 0 {
            return Err(Error::from_hresult(HRESULT::from_win32(
                ERROR_INVALID_DATA.0,
            )));
        }
        // SAFETY: the preceding branch established that `count` is non-null
        // and that the kernel-reported subauthority count is positive.
        let index = unsafe { (*count - 1) as u32 };
        // SAFETY: `index` is the last subauthority reported by Windows.
        let rid = unsafe { GetSidSubAuthority(label.Label.Sid, index) };
        if rid.is_null() {
            return Err(Error::from_hresult(HRESULT::from_win32(
                ERROR_INVALID_DATA.0,
            )));
        }
        // SAFETY: `rid` is non-null and points into the same live SID buffer.
        Ok(unsafe { *rid })
    }
}

impl Drop for ProcessToken {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: the handle came from `OpenProcessToken` and is closed
            // exactly once, here.
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

/// Converts a SID to its `S-1-…` string form.
fn sid_to_string(sid: PSID) -> Result<String> {
    let mut text = PWSTR::null();
    // SAFETY: `sid` points into a token-information buffer that outlives the
    // call; `text` is a valid out-parameter that receives `LocalAlloc`ed
    // memory we free below.
    unsafe {
        ConvertSidToStringSidW(sid, &mut text)?;
    }
    // SAFETY: on success `text` is a NUL-terminated wide string.
    let owned = unsafe { text.to_string() };
    // SAFETY: `text` came from `ConvertSidToStringSidW`, which documents its
    // result as `LocalAlloc`ed memory the caller frees with `LocalFree`.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(text.as_ptr().cast())));
    }
    owned.map_err(|_| Error::from_hresult(win32_hresult(ERROR_INSUFFICIENT_BUFFER)))
}

fn win32_hresult(code: WIN32_ERROR) -> HRESULT {
    HRESULT::from_win32(code.0)
}

/// UTF-16 with a trailing NUL, for the pointer-only Win32 APIs above.
pub(super) fn to_wide_nul(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}
