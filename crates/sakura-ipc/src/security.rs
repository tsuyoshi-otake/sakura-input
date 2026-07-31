//! The pipe's name and the security descriptor that guards it (DESIGN 7).
//!
//! This is the single most load-bearing piece of Win32 in the project. The
//! architecture of DESIGN 3 — a thin DLL inside every host process talking
//! to one engine outside — only works if a *sandboxed* host can reach the
//! pipe. Chrome's renderers, Edge's content processes and every UWP app run
//! in an AppContainer at low integrity, and each of the three mechanisms
//! below has to be right or those hosts silently lose their IME.
//!
//! # 1. The name
//!
//! `\\.\pipe\sakura_input_<logon_sid>`. The logon SID is unique per logon
//! session, so two users signed in at once — or the same user on the console
//! and over RDP — get one engine each rather than fighting over one pipe.
//! The user SID alone would not do that.
//!
//! # 2. The DACL
//!
//! Three principals are granted access:
//!
//! - the current user, in full, because the engine has to create further
//!   instances of its own pipe;
//! - `ALL APPLICATION PACKAGES` (S-1-15-2-1), which is what an AppContainer
//!   token is checked against — its user SID is present but inert, since an
//!   AppContainer access check is the *intersection* of the normal check
//!   and a second one that counts only package and capability SIDs;
//! - `ALL RESTRICTED APPLICATION PACKAGES` (S-1-15-2-2), for the
//!   less-privileged AppContainers (LPAC) that Chromium's renderers use.
//!   An LPAC token is *not* granted access by S-1-15-2-1, so leaving this
//!   ACE out breaks precisely the browser everyone tests with.
//!
//! Sandboxed principals get [`CLIENT_ACCESS`] rather than generic rights,
//! and the reason is a genuine Win32 trap: for a named pipe,
//! `FILE_APPEND_DATA` and `FILE_CREATE_PIPE_INSTANCE` are the same bit
//! (0x0004). `GENERIC_WRITE` expands to include it, so granting `GRGW`
//! would let any sandboxed process create another instance of our pipe and
//! collect connections meant for us. The consequence runs the other way
//! too, and it is the part that bites: because we do *not* grant that bit,
//! a client asking for `GENERIC_READ | GENERIC_WRITE` is denied outright.
//! Clients must ask for [`CLIENT_ACCESS`] exactly. That constant is public
//! so the DLL uses the one the server actually grants instead of a
//! plausible-looking mask that fails only inside a sandbox.
//!
//! # 3. The mandatory label
//!
//! An object with no label defaults to medium integrity, and a low-integrity
//! subject cannot write to a medium object no matter what the DACL says —
//! integrity is checked *in addition to* the DACL, not instead of it. This
//! is the single most common named-pipe-versus-sandbox bug. `S:(ML;;NW;;;LW)`
//! labels the pipe low with no-write-up, which permits every subject at low
//! or above: sandboxed clients, ordinary medium-integrity apps, and
//! elevated ones alike.
//!
//! # What is deliberately not here
//!
//! No ACE for SYSTEM or Administrators. Nothing in Sakura Input runs as
//! either, and an unused grant is only an attack surface.

use std::ffi::c_void;

use windows::core::{Error, Result, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_INSUFFICIENT_BUFFER, HANDLE, HLOCAL, WIN32_ERROR,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, TokenGroups, TokenUser, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    TOKEN_GROUPS, TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The prefix every instance of the pipe shares.
const PIPE_PREFIX: &str = r"\\.\pipe\sakura_input_";

/// `SE_GROUP_LOGON_ID` from `winnt.h`: marks the one group SID in a token
/// that identifies the logon session.
///
/// Spelled out here rather than taken from the bindings because the exact
/// path and type of this constant has moved between `windows` releases, and
/// a numeric constant from a header is not going to change.
const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;

/// `S-1-15-2-1`, the group every AppContainer token carries.
const ALL_APPLICATION_PACKAGES: &str = "S-1-15-2-1";

/// `S-1-15-2-2`, carried by less-privileged AppContainers instead of
/// [`ALL_APPLICATION_PACKAGES`]. Written as a literal SID rather than the
/// `RAP` alias, which older SDDL parsers do not know.
const ALL_RESTRICTED_APPLICATION_PACKAGES: &str = "S-1-15-2-2";

/// The exact access mask a client must request when opening the pipe.
///
/// `FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES |
/// FILE_WRITE_ATTRIBUTES | SYNCHRONIZE`. Deliberately *not* `GENERIC_READ |
/// GENERIC_WRITE`: see the module docs — the generic mapping drags in
/// `FILE_CREATE_PIPE_INSTANCE`, which the server does not grant, so a
/// client asking generically is denied.
pub const CLIENT_ACCESS: u32 = 0x0010_0183;

/// The pipe this engine serves and its clients connect to.
///
/// Falls back to the user SID when the token carries no logon SID, which
/// happens for some non-interactive token types. That is still correct: a
/// token with no logon session has no second interactive session to collide
/// with.
pub fn pipe_name() -> Result<String> {
    let token = ProcessToken::open()?;
    let sid = match token.logon_sid()? {
        Some(sid) => sid,
        None => token.user_sid()?,
    };
    Ok(format!("{PIPE_PREFIX}{sid}"))
}

/// The SDDL text describing the pipe's security descriptor.
///
/// Returned as a string rather than a built descriptor so it can be
/// computed once and handed to every server thread: a `String` crosses a
/// thread boundary freely, a raw `PSECURITY_DESCRIPTOR` does not. Each
/// thread turns it into a [`Descriptor`] of its own.
pub fn sddl() -> Result<String> {
    let user = ProcessToken::open()?.user_sid()?;
    Ok(format!(
        "D:P(A;;FA;;;{user})\
         (A;;0x{CLIENT_ACCESS:08X};;;{ALL_APPLICATION_PACKAGES})\
         (A;;0x{CLIENT_ACCESS:08X};;;{ALL_RESTRICTED_APPLICATION_PACKAGES})\
         S:(ML;;NW;;;LW)"
    ))
}

/// A security descriptor built from SDDL, freed on drop.
pub struct Descriptor {
    raw: PSECURITY_DESCRIPTOR,
}

impl Descriptor {
    /// Parses an SDDL string into a descriptor.
    pub fn from_sddl(sddl: &str) -> Result<Self> {
        let wide = to_wide_nul(sddl);
        let mut raw = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `wide` is NUL-terminated and outlives the call; `raw` is a
        // valid out-parameter. The size out-parameter is optional and we do
        // not need the length. On success the descriptor is `LocalAlloc`ed
        // and owned by us, which `Drop` honours.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide.as_ptr()),
                SDDL_REVISION_1,
                &mut raw,
                None,
            )?;
        }
        Ok(Descriptor { raw })
    }

    /// The descriptor built from [`sddl`].
    pub fn for_pipe() -> Result<Self> {
        Self::from_sddl(&sddl()?)
    }

    /// A `SECURITY_ATTRIBUTES` pointing at this descriptor.
    ///
    /// Borrows `self`, because the returned struct holds a raw pointer that
    /// `Drop` invalidates. `bInheritHandle` is false: a child process
    /// inheriting a live pipe instance would be handed a connection it was
    /// never authenticated for.
    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: core::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.raw.0,
            bInheritHandle: false.into(),
        }
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        if !self.raw.0.is_null() {
            // SAFETY: `raw` came from
            // `ConvertStringSecurityDescriptorToSecurityDescriptorW`, which
            // documents its result as `LocalAlloc`ed memory the caller frees
            // with `LocalFree`. It is freed exactly once, here.
            unsafe {
                let _ = LocalFree(Some(HLOCAL(self.raw.0)));
            }
            self.raw = PSECURITY_DESCRIPTOR::default();
        }
    }
}

impl core::fmt::Debug for Descriptor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Descriptor").finish_non_exhaustive()
    }
}

/// The current process's access token, closed on drop.
struct ProcessToken {
    handle: HANDLE,
}

impl ProcessToken {
    fn open() -> Result<Self> {
        let mut handle = HANDLE::default();
        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
        // release, and `handle` is a valid out-parameter.
        unsafe {
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut handle)?;
        }
        Ok(ProcessToken { handle })
    }

    /// The token's user SID, as a string.
    fn user_sid(&self) -> Result<String> {
        let buf = self.information(TokenUser)?;
        // SAFETY: `information` filled `buf` with a `TOKEN_USER` for this
        // class, and the buffer is `u64`-aligned so the struct's pointer
        // field is correctly aligned.
        let sid = unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid };
        sid_to_string(sid)
    }

    /// The token's logon-session SID, if it has one.
    fn logon_sid(&self) -> Result<Option<String>> {
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
fn to_wide_nul(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pipe_name_is_scoped_to_the_logon_session() {
        let name = pipe_name().expect("the current process has a token");
        assert!(name.starts_with(PIPE_PREFIX), "unexpected name: {name}");
        let sid = &name[PIPE_PREFIX.len()..];
        assert!(sid.starts_with("S-1-"), "not a SID: {sid}");
        // A pipe name is capped at 256 characters after the prefix; a SID is
        // nowhere near that, but the assertion documents the limit.
        assert!(sid.len() < 256);
    }

    #[test]
    fn the_descriptor_grants_the_two_sandbox_groups_and_labels_the_pipe_low() {
        let text = sddl().expect("the current process has a token");
        assert!(text.contains(ALL_APPLICATION_PACKAGES), "{text}");
        assert!(
            text.contains(ALL_RESTRICTED_APPLICATION_PACKAGES),
            "LPAC clients (Chromium renderers) would be locked out: {text}"
        );
        assert!(
            text.contains("S:(ML;;NW;;;LW)"),
            "without the low-integrity label every sandboxed writer is \
             rejected regardless of the DACL: {text}"
        );
    }

    /// The bit that makes generic access rights unusable here.
    #[test]
    fn the_client_mask_withholds_the_create_instance_right() {
        const FILE_CREATE_PIPE_INSTANCE: u32 = 0x0004;
        assert_eq!(
            CLIENT_ACCESS & FILE_CREATE_PIPE_INSTANCE,
            0,
            "a sandboxed client could create a rival instance of our pipe"
        );
        // What it does grant: read, write, both attribute rights, and the
        // synchronize right every blocking wait needs.
        assert_eq!(
            CLIENT_ACCESS,
            0x0001 | 0x0002 | 0x0080 | 0x0100 | 0x0010_0000
        );
    }

    #[test]
    fn the_sddl_parses_into_a_real_descriptor() {
        let descriptor = Descriptor::for_pipe().expect("the SDDL must be well formed");
        let attributes = descriptor.attributes();
        assert!(!attributes.lpSecurityDescriptor.is_null());
        assert_eq!(
            attributes.nLength as usize,
            size_of::<SECURITY_ATTRIBUTES>()
        );
        assert!(
            !attributes.bInheritHandle.as_bool(),
            "an inheritable handle would hand the pipe to every child process"
        );
    }

    #[test]
    fn malformed_sddl_is_an_error_rather_than_a_panic() {
        assert!(Descriptor::from_sddl("this is not a security descriptor").is_err());
    }
}
