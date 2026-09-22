//! Client admission: the pipe name, its security descriptor, the access mask
//! clients request, and the kernel-observed classification of a connected
//! client (module docs of `security` explain the three mechanisms).

use windows::core::{Error, Result, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{LocalFree, ERROR_SUCCESS, HANDLE, HLOCAL, WIN32_ERROR};
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW, GetSecurityInfo,
    SetEntriesInAclW, SetSecurityInfo, EXPLICIT_ACCESS_W, GRANT_ACCESS, SDDL_REVISION_1,
    SE_KERNEL_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP, TRUSTEE_W,
};
use windows::Win32::Security::{
    TokenIsAppContainer, ACL, DACL_SECURITY_INFORMATION, NO_INHERITANCE, PSECURITY_DESCRIPTOR,
    PSID, SECURITY_ATTRIBUTES, TOKEN_ACCESS_MASK, TOKEN_QUERY,
};
use windows::Win32::System::SystemServices::SECURITY_MANDATORY_MEDIUM_RID;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

use super::process::{to_wide_nul, ProcessHandle, ProcessToken};

/// The prefix every instance of the pipe shares.
pub(super) const PIPE_PREFIX: &str = r"\\.\pipe\sakura_input_";

/// The three server-owned IPC boundaries.
///
/// The endpoint is part of the pipe name and security descriptor selected by
/// the engine. It is intentionally not negotiated in `Request::Hello`: a
/// low-integrity client must not be able to claim a renderer or control role
/// by changing a field in an otherwise valid protocol frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Endpoint {
    /// TSF keystrokes and document/session operations. This is the only
    /// endpoint reachable from an AppContainer host.
    Data,
    /// Renderer long-poll and opaque UI actions.
    Renderer,
    /// Per-user settings and installer/regtool administration.
    Control,
}

impl Endpoint {
    /// Stable suffix used in the named-pipe object name.
    pub const fn suffix(self) -> &'static str {
        match self {
            Self::Data => "data",
            Self::Renderer => "renderer",
            Self::Control => "control",
        }
    }
}

/// `S-1-15-2-1`, the group every AppContainer token carries.
pub(super) const ALL_APPLICATION_PACKAGES: &str = "S-1-15-2-1";

/// `S-1-15-2-2`, carried by less-privileged AppContainers instead of
/// [`ALL_APPLICATION_PACKAGES`]. Written as a literal SID rather than the
/// `RAP` alias, which older SDDL parsers do not know.
pub(super) const ALL_RESTRICTED_APPLICATION_PACKAGES: &str = "S-1-15-2-2";

/// The exact access mask a client must request when opening the pipe.
///
/// `FILE_READ_DATA | FILE_WRITE_DATA | FILE_READ_ATTRIBUTES |
/// FILE_WRITE_ATTRIBUTES | SYNCHRONIZE`. Deliberately *not* `GENERIC_READ |
/// GENERIC_WRITE`: see the module docs — the generic mapping drags in
/// `FILE_CREATE_PIPE_INSTANCE`, which the server does not grant, so a
/// client asking generically is denied.
pub const CLIENT_ACCESS: u32 = 0x0010_0183;

/// The server's kernel-observed classification of a connected client.
///
/// This is not a wire claim. The engine derives it from the client PID
/// returned by the accepted pipe handle and the process token. `Unknown` is
/// intentionally useful: ordinary data requests may continue, but sensitive
/// AI requests must fail closed when this classification cannot be obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientTrust {
    /// A non-AppContainer process at medium integrity or above.
    MediumOrHigher,
    /// A process whose token integrity is below medium.
    LowIntegrity,
    /// An AppContainer token, regardless of its integrity RID.
    AppContainer,
    /// Token/process inspection failed or returned malformed data.
    Unknown,
}

/// Classifies a connected client from its kernel-reported process ID.
///
/// A failure is returned instead of being guessed as medium. The engine maps
/// that failure to [`ClientTrust::Unknown`] and denies AI operations for it.
pub fn classify_client_process(process_id: u32) -> Result<ClientTrust> {
    let process = ProcessHandle::open(process_id)?;
    let token = ProcessToken::open_process(&process)?;
    classify_token(&token)
}

/// Grants sandboxed clients the two read-only kernel queries required to
/// authenticate this engine after connecting to the low-integrity data pipe.
///
/// AppContainer restricted-token access checks do not accept the ordinary
/// current-user ACE on process/token objects. Without these narrow grants an
/// AppContainer can open the data pipe but cannot prove that its server PID is
/// this medium-integrity engine. Existing DACL entries are preserved; no VM,
/// terminate, duplicate-handle, write, or impersonation right is added.
pub fn allow_sandbox_identity_queries() -> Result<()> {
    // SAFETY: the current-process pseudo handle is always valid and is not
    // closed. Only its DACL is augmented with a read-only query right.
    let current_process = unsafe { GetCurrentProcess() };
    grant_sandbox_query_access(current_process, PROCESS_QUERY_LIMITED_INFORMATION.0)?;

    const READ_CONTROL_ACCESS: u32 = 0x0002_0000;
    const WRITE_DAC_ACCESS: u32 = 0x0004_0000;
    let mut token_handle = HANDLE::default();
    // SAFETY: GetCurrentProcess is a valid pseudo handle and `token_handle`
    // receives one owned token handle. READ_CONTROL/WRITE_DAC are needed only
    // while preserving and extending this token object's DACL.
    unsafe {
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ACCESS_MASK(TOKEN_QUERY.0 | READ_CONTROL_ACCESS | WRITE_DAC_ACCESS),
            &mut token_handle,
        )?;
    }
    let token = ProcessToken {
        handle: token_handle,
    };
    grant_sandbox_query_access(token.handle, TOKEN_QUERY.0)
}

fn grant_sandbox_query_access(handle: HANDLE, access: u32) -> Result<()> {
    let mut old_dacl: *mut ACL = core::ptr::null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: all outputs are valid writable pointers. `descriptor` owns the
    // allocation returned by GetSecurityInfo and is released below.
    status_result(unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(&mut old_dacl),
            None,
            Some(&mut descriptor),
        )
    })?;
    let descriptor_guard = LocalAllocation(HLOCAL(descriptor.0));

    let app_packages = LocalSid::from_string(ALL_APPLICATION_PACKAGES)?;
    let restricted_packages = LocalSid::from_string(ALL_RESTRICTED_APPLICATION_PACKAGES)?;
    let entries = [
        sandbox_query_entry(app_packages.0, access),
        sandbox_query_entry(restricted_packages.0, access),
    ];
    let mut new_dacl: *mut ACL = core::ptr::null_mut();
    // SAFETY: both SID allocations and the old descriptor remain alive for
    // the call. SetEntriesInAclW returns a LocalAlloc-owned ACL.
    status_result(unsafe { SetEntriesInAclW(Some(&entries), Some(old_dacl), &mut new_dacl) })?;
    let new_dacl_guard = LocalAllocation(HLOCAL(new_dacl.cast()));

    // SAFETY: the target handle is live and `new_dacl` remains allocated for
    // the synchronous SetSecurityInfo call. Existing owner/group/SACL values
    // are intentionally untouched.
    let result = status_result(unsafe {
        SetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            None,
            None,
            Some(new_dacl),
            None,
        )
    });
    drop(new_dacl_guard);
    drop(descriptor_guard);
    result
}

fn sandbox_query_entry(sid: PSID, access: u32) -> EXPLICIT_ACCESS_W {
    EXPLICIT_ACCESS_W {
        grfAccessPermissions: access,
        grfAccessMode: GRANT_ACCESS,
        grfInheritance: NO_INHERITANCE,
        Trustee: TRUSTEE_W {
            pMultipleTrustee: core::ptr::null_mut(),
            MultipleTrusteeOperation: Default::default(),
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_WELL_KNOWN_GROUP,
            ptstrName: PWSTR(sid.0.cast()),
        },
    }
}

fn status_result(status: WIN32_ERROR) -> Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(Error::from_hresult(HRESULT::from_win32(status.0)))
    }
}

struct LocalSid(PSID);

impl LocalSid {
    fn from_string(value: &str) -> Result<Self> {
        let wide = to_wide_nul(value);
        let mut sid = PSID::default();
        // SAFETY: `wide` is NUL-terminated and `sid` is a valid output. The
        // returned LocalAlloc allocation is owned by this wrapper.
        unsafe { ConvertStringSidToSidW(PCWSTR(wide.as_ptr()), &mut sid)? };
        Ok(Self(sid))
    }
}

impl Drop for LocalSid {
    fn drop(&mut self) {
        // SAFETY: ConvertStringSidToSidW returned a LocalAlloc allocation.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(self.0 .0)));
        }
    }
}

struct LocalAllocation(HLOCAL);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0 .0.is_null() {
            // SAFETY: this wrapper owns one allocation returned by a Win32
            // security API documented to use LocalAlloc.
            unsafe {
                let _ = LocalFree(Some(self.0));
            }
        }
    }
}

/// The pipe this engine serves and its clients connect to.
///
/// Falls back to the user SID when the token carries no logon SID, which
/// happens for some non-interactive token types. That is still correct: a
/// token with no logon session has no second interactive session to collide
/// with.
pub fn pipe_name() -> Result<String> {
    pipe_name_for(Endpoint::Data)
}

/// Resolves the pipe name for one server-owned endpoint in this logon session.
pub fn pipe_name_for(endpoint: Endpoint) -> Result<String> {
    let token = ProcessToken::open()?;
    let sid = match token.logon_sid()? {
        Some(sid) => sid,
        None => token.user_sid()?,
    };
    // Keep the historical data-plane name unsuffixed so an older TSF DLL
    // loaded by a long-lived host remains compatible with a newly installed
    // engine. New renderer/control boundaries are suffixed and therefore do
    // not change the legacy data contract.
    if endpoint == Endpoint::Data {
        Ok(format!("{PIPE_PREFIX}{sid}"))
    } else {
        Ok(format!("{PIPE_PREFIX}{sid}_{}", endpoint.suffix()))
    }
}

/// The SDDL text describing the pipe's security descriptor.
///
/// Returned as a string rather than a built descriptor so it can be
/// computed once and handed to every server thread: a `String` crosses a
/// thread boundary freely, a raw `PSECURITY_DESCRIPTOR` does not. Each
/// thread turns it into a [`Descriptor`] of its own.
pub fn sddl() -> Result<String> {
    sddl_for(Endpoint::Data)
}

/// Returns the security descriptor for one server-owned endpoint.
///
/// Only the data plane carries the low-integrity/AppContainer grants. The
/// renderer and control planes are medium-integrity user processes and omit
/// those ACEs; a low-integrity process therefore cannot open either object.
pub fn sddl_for(endpoint: Endpoint) -> Result<String> {
    let user = ProcessToken::open()?.user_sid()?;
    match endpoint {
        Endpoint::Data => Ok(format!(
            "D:P(A;;FA;;;{user})\
             (A;;0x{CLIENT_ACCESS:08X};;;{ALL_APPLICATION_PACKAGES})\
             (A;;0x{CLIENT_ACCESS:08X};;;{ALL_RESTRICTED_APPLICATION_PACKAGES})\
             S:(ML;;NW;;;LW)"
        )),
        Endpoint::Renderer | Endpoint::Control => Ok(format!(
            // The explicit medium mandatory label is intentional. Leaving
            // the label implicit would make this boundary depend on the
            // process default token rather than on the endpoint contract.
            "D:P(A;;FA;;;{user})S:(ML;;NW;;;ME)"
        )),
    }
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

pub(super) fn classify_token(token: &ProcessToken) -> Result<ClientTrust> {
    let app_container = token.scalar(TokenIsAppContainer)? != 0;
    let integrity = token.integrity_rid()?;
    if app_container {
        Ok(ClientTrust::AppContainer)
    } else if integrity < SECURITY_MANDATORY_MEDIUM_RID as u32 {
        Ok(ClientTrust::LowIntegrity)
    } else {
        Ok(ClientTrust::MediumOrHigher)
    }
}
