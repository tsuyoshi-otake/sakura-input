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

use std::ffi::{c_void, OsString};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};

use windows::core::{Error, Result, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_DATA, ERROR_SUCCESS, HANDLE,
    HLOCAL, WIN32_ERROR,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    ConvertStringSidToSidW, GetSecurityInfo, SetEntriesInAclW, SetSecurityInfo, EXPLICIT_ACCESS_W,
    GRANT_ACCESS, SDDL_REVISION_1, SE_KERNEL_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_WELL_KNOWN_GROUP,
    TRUSTEE_W,
};
use windows::Win32::Security::{
    GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation, TokenGroups,
    TokenIntegrityLevel, TokenIsAppContainer, TokenUser, ACL, DACL_SECURITY_INFORMATION,
    NO_INHERITANCE, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_ACCESS_MASK,
    TOKEN_GROUPS, TOKEN_INFORMATION_CLASS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER,
};
use windows::Win32::System::SystemServices::SECURITY_MANDATORY_MEDIUM_RID;
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, QueryFullProcessImageNameW,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// The prefix every instance of the pipe shares.
const PIPE_PREFIX: &str = r"\\.\pipe\sakura_input_";

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

/// The process identity a production client will accept on a pipe handle.
///
/// This is deliberately a typed policy rather than a string prefix. A
/// versioned install may have an older TSF DLL talking to a newly installed
/// engine, so callers use [`InstalledRoot`] for production. [`Exact`] is for
/// ownership-safe tests and diagnostics where one image is intentionally
/// fixed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerTrustPolicy {
    /// Accept exactly one canonical image path.
    Exact(PathBuf),
    /// Accept `root\\versions\\<one direct release directory>\\sakura_engine.exe`.
    InstalledRoot(PathBuf),
}

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

const ENGINE_IMAGE_NAME: &str = "sakura_engine.exe";
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

impl ServerTrustPolicy {
    /// Returns whether a queried image path satisfies this policy.
    ///
    /// Both sides are canonicalized and every existing component is checked
    /// for `FILE_ATTRIBUTE_REPARSE_POINT`. Lexical `..` is rejected before
    /// canonicalization so a policy cannot accidentally become a broad path
    /// check. `Exact` also has a strictly-equal lexical fallback for
    /// AppContainer tests/diagnostics: restricted tokens may query the engine
    /// process but have no filesystem traversal right to a developer target
    /// directory. Production uses `InstalledRoot`, which never takes that
    /// fallback. No textual-prefix comparison is used.
    pub fn matches_image_path(&self, image: &Path) -> bool {
        match self {
            Self::Exact(expected) => {
                match (
                    canonical_non_reparse(expected),
                    canonical_non_reparse(image),
                ) {
                    (Some(expected), Some(image)) => {
                        is_engine_image(&image) && same_windows_path(&expected, &image)
                    }
                    _ => {
                        lexically_safe_engine_path(expected)
                            && lexically_safe_engine_path(image)
                            && same_windows_path(expected, image)
                    }
                }
            }
            Self::InstalledRoot(root) => {
                let Some(image) = canonical_non_reparse(image) else {
                    return false;
                };
                installed_layout_matches(root, &image)
            }
        }
    }
}

/// Why a verified connection refused its server.
///
/// The caller used to reduce this to `.is_err()`, which is how Issue #104 — a
/// CI rejection that has now happened four times — produced no evidence at all
/// beyond "rejected". Every variant names exactly one call or one policy
/// decision and carries nothing but an OS error code, so it is safe to print
/// wherever the fault itself is printed.
///
/// The distinction that matters is between a question answered "no" and a
/// question that could not be asked: `ImagePathRejected` means the observed
/// path did not satisfy policy, while `ImagePathUnreadable` means we never
/// obtained an image path. Rejection alone does not identify which lexical,
/// filesystem or equality condition failed, or prove a different executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerRejection {
    /// The trust policy itself could not be built, so nothing was verified.
    /// The connection is refused because the question could not be put, not
    /// because the peer failed it.
    PolicyUnavailable,
    /// The kernel reported no usable server process for this pipe handle.
    NoServerProcessId,
    /// `OpenProcess` failed for the kernel-reported server PID.
    ProcessUnopenable(HRESULT),
    /// `QueryFullProcessImageNameW` failed for that process.
    ImagePathUnreadable(HRESULT),
    /// The image path was read, and it is not one this policy accepts.
    ImagePathRejected,
    /// `OpenProcessToken` failed for that process.
    TokenUnopenable(HRESULT),
    /// The token was opened but could not be classified.
    TokenUnclassifiable(HRESULT),
    /// The token classified below medium integrity.
    IntegrityRejected,
}

impl core::fmt::Display for ServerRejection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PolicyUnavailable => write!(f, "trust policy unavailable"),
            Self::NoServerProcessId => write!(f, "no server process id"),
            Self::ProcessUnopenable(code) => write!(f, "OpenProcess failed ({code:?})"),
            Self::ImagePathUnreadable(code) => {
                write!(f, "QueryFullProcessImageNameW failed ({code:?})")
            }
            Self::ImagePathRejected => write!(f, "image path is not the one the policy accepts"),
            Self::TokenUnopenable(code) => write!(f, "OpenProcessToken failed ({code:?})"),
            Self::TokenUnclassifiable(code) => write!(f, "token classification failed ({code:?})"),
            Self::IntegrityRejected => write!(f, "token integrity below medium"),
        }
    }
}

/// Verifies the process attached to one exact client pipe handle.
///
/// The caller must obtain `process_id` from `GetNamedPipeServerProcessId` on
/// that same handle. The image path and token are then read from the kernel;
/// a peer cannot satisfy this contract by sending a forged Hello field.
///
/// The refusal is returned as a [`ServerRejection`] rather than an `Error`
/// because every step below can fail for `ERROR_ACCESS_DENIED`, so an HRESULT
/// alone cannot say which one did. What the caller does is unchanged: any
/// `Err` refuses the connection.
pub fn verify_server_process(
    process_id: u32,
    policy: &ServerTrustPolicy,
) -> core::result::Result<(), ServerRejection> {
    let process = ProcessHandle::open(process_id)
        .map_err(|error| ServerRejection::ProcessUnopenable(error.code()))?;
    let image = process
        .image_path()
        .map_err(|error| ServerRejection::ImagePathUnreadable(error.code()))?;
    if !policy.matches_image_path(&image) {
        return Err(ServerRejection::ImagePathRejected);
    }
    let token = ProcessToken::open_process(&process)
        .map_err(|error| ServerRejection::TokenUnopenable(error.code()))?;
    let trust = classify_token(&token)
        .map_err(|error| ServerRejection::TokenUnclassifiable(error.code()))?;
    if trust != ClientTrust::MediumOrHigher {
        return Err(ServerRejection::IntegrityRejected);
    }
    Ok(())
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

fn installed_layout_matches(root: &Path, image: &Path) -> bool {
    let Some(root) = canonical_non_reparse(root) else {
        return false;
    };
    let Some(versions) = canonical_non_reparse(&root.join("versions")) else {
        return false;
    };
    let Some(parent) = image.parent() else {
        return false;
    };
    let Some(release_name) = parent.file_name() else {
        return false;
    };
    let Some(version_parent) = parent.parent() else {
        return false;
    };
    // Comparing the canonical directory itself, rather than the original
    // strings, prevents both sibling-prefix confusion and `..` aliases.
    if !same_windows_path(version_parent, &versions) {
        return false;
    }
    !release_name.is_empty() && is_engine_image(image)
}

fn is_engine_image(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        name.to_string_lossy()
            .eq_ignore_ascii_case(ENGINE_IMAGE_NAME)
    })
}

fn lexically_safe_engine_path(path: &Path) -> bool {
    path.is_absolute()
        && is_engine_image(path)
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
}

fn same_windows_path(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .eq_ignore_ascii_case(&right.to_string_lossy())
}

fn canonical_non_reparse(path: &Path) -> Option<PathBuf> {
    if contains_parent_component(path) || reject_reparse_components(path).is_err() {
        return None;
    }
    let canonical = std::fs::canonicalize(path).ok()?;
    if reject_reparse_components(&canonical).is_err() {
        return None;
    }
    Some(canonical)
}

fn contains_parent_component(path: &Path) -> bool {
    path.components()
        .any(|component| matches!(component, Component::ParentDir))
}

fn reject_reparse_components(path: &Path) -> std::io::Result<()> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => return Err(std::io::ErrorKind::InvalidInput.into()),
            Component::Normal(name) => {
                current.push(name);
                let metadata = std::fs::symlink_metadata(&current)?;
                if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "reparse-point path is not trusted",
                    ));
                }
            }
        }
    }
    Ok(())
}

/// A process handle opened only for the two read-only identity queries used
/// by this module.
struct ProcessHandle {
    handle: HANDLE,
}

impl ProcessHandle {
    fn open(process_id: u32) -> Result<Self> {
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

    fn image_path(&self) -> Result<PathBuf> {
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
                PROCESS_NAME_WIN32,
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

    /// The descriptor for a named-pipe endpoint.
    pub fn for_endpoint(endpoint: Endpoint) -> Result<Self> {
        Self::from_sddl(&sddl_for(endpoint)?)
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

    fn open_process(process: &ProcessHandle) -> Result<Self> {
        let mut handle = HANDLE::default();
        // SAFETY: `process.handle` is live and only queried for its token;
        // `handle` is a valid output location owned by this wrapper.
        unsafe {
            OpenProcessToken(process.handle, TOKEN_QUERY, &mut handle)?;
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

fn classify_token(token: &ProcessToken) -> Result<ClientTrust> {
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

impl ProcessToken {
    fn scalar(&self, class: TOKEN_INFORMATION_CLASS) -> Result<u32> {
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

    fn integrity_rid(&self) -> Result<u32> {
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
fn to_wide_nul(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::PipeInstance;
    use std::mem::size_of;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{
        CloseHandle, LocalFree, HANDLE, HLOCAL, INVALID_HANDLE_VALUE,
    };
    use windows::Win32::Security::Authorization::ConvertStringSidToSidW;
    use windows::Win32::Security::{
        DuplicateTokenEx, GetLengthSid, GetTokenInformation, ImpersonateLoggedOnUser, RevertToSelf,
        SecurityImpersonation, SetTokenInformation, TokenImpersonation, TokenIntegrityLevel,
        TokenIsAppContainer, PSID, SID_AND_ATTRIBUTES, TOKEN_ADJUST_DEFAULT, TOKEN_DUPLICATE,
        TOKEN_IMPERSONATE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
    };
    use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows::Win32::System::Pipes::{
        CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    #[test]
    fn the_pipe_name_is_scoped_to_the_logon_session() {
        let name = pipe_name().expect("the current process has a token");
        assert!(name.starts_with(PIPE_PREFIX), "unexpected name: {name}");
        assert!(
            !name.ends_with("_data"),
            "data endpoint keeps its legacy name: {name}"
        );
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

    #[test]
    fn only_the_data_endpoint_grants_sandbox_groups() {
        let data = sddl_for(Endpoint::Data).expect("data descriptor");
        let renderer = sddl_for(Endpoint::Renderer).expect("renderer descriptor");
        let control = sddl_for(Endpoint::Control).expect("control descriptor");
        assert!(data.contains(ALL_APPLICATION_PACKAGES));
        assert!(data.contains(ALL_RESTRICTED_APPLICATION_PACKAGES));
        assert!(!renderer.contains(ALL_APPLICATION_PACKAGES));
        assert!(!renderer.contains(ALL_RESTRICTED_APPLICATION_PACKAGES));
        assert!(!control.contains(ALL_APPLICATION_PACKAGES));
        assert!(!control.contains(ALL_RESTRICTED_APPLICATION_PACKAGES));
        assert!(renderer.contains("S:(ML;;NW;;;ME)"));
        assert!(control.contains("S:(ML;;NW;;;ME)"));
    }

    #[test]
    fn endpoint_names_are_distinct_and_stable() {
        let data = pipe_name_for(Endpoint::Data).expect("data name");
        let renderer = pipe_name_for(Endpoint::Renderer).expect("renderer name");
        let control = pipe_name_for(Endpoint::Control).expect("control name");
        assert_ne!(data, renderer);
        assert_ne!(data, control);
        assert_ne!(renderer, control);
        assert!(renderer.ends_with("_renderer"));
        assert!(control.ends_with("_control"));
    }

    #[test]
    fn installed_root_policy_uses_one_direct_release_directory() {
        let root = std::env::temp_dir().join(format!(
            "sakura-ipc-trust-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let engine = root
            .join("versions")
            .join("release-a")
            .join(ENGINE_IMAGE_NAME);
        std::fs::create_dir_all(engine.parent().expect("release parent")).expect("directories");
        std::fs::write(&engine, b"engine").expect("image fixture");

        let policy = ServerTrustPolicy::InstalledRoot(root.clone());
        assert!(policy.matches_image_path(&engine));
        assert!(!policy.matches_image_path(
            &root
                .join("versions")
                .join("release-a")
                .join("sakura_engine.exe.bak")
        ));
        assert!(!policy.matches_image_path(
            &root
                .join("versions")
                .join("release-a")
                .join("..")
                .join(ENGINE_IMAGE_NAME)
        ));

        let sibling = root.with_file_name(format!(
            "{}-sibling",
            root.file_name().unwrap().to_string_lossy()
        ));
        let sibling_engine = sibling
            .join("versions")
            .join("release-a")
            .join(ENGINE_IMAGE_NAME);
        std::fs::create_dir_all(sibling_engine.parent().expect("sibling parent"))
            .expect("sibling directories");
        std::fs::write(&sibling_engine, b"sibling").expect("sibling image");
        assert!(!policy.matches_image_path(&sibling_engine));

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&sibling);
    }

    #[test]
    fn trust_policy_rejects_reparse_root_when_the_platform_allows_a_test_link() {
        let root = std::env::temp_dir().join(format!(
            "sakura-ipc-reparse-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let engine = root
            .join("versions")
            .join("release-a")
            .join(ENGINE_IMAGE_NAME);
        std::fs::create_dir_all(engine.parent().expect("release parent")).expect("directories");
        std::fs::write(&engine, b"engine").expect("image fixture");
        let alias = root.with_file_name(format!(
            "{}-alias",
            root.file_name().unwrap().to_string_lossy()
        ));
        let linked = std::os::windows::fs::symlink_dir(&root, &alias);
        if linked.is_ok() {
            let alias_engine = alias
                .join("versions")
                .join("release-a")
                .join(ENGINE_IMAGE_NAME);
            assert!(
                !ServerTrustPolicy::InstalledRoot(alias.clone()).matches_image_path(&alias_engine)
            );
        }
        let _ = std::fs::remove_dir_all(&alias);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn current_process_token_is_medium_or_higher_and_not_appcontainer() {
        let process = ProcessHandle::open(std::process::id()).expect("current process");
        let token = ProcessToken::open_process(&process).expect("current token");
        assert_eq!(classify_token(&token), Ok(ClientTrust::MediumOrHigher));
    }

    /// Documents the production ACL's remaining rival-instance property with
    /// a real, non-AppContainer low-integrity impersonation token. The current
    /// user's `FA` ACE plus the low mandatory label intentionally permits this
    /// second server instance; production clients therefore require the
    /// kernel PID/image/integrity binding in `connect_verified_to`.
    #[test]
    fn low_integrity_non_appcontainer_can_add_a_rival_instance_under_acl() {
        let name = format!(
            r"\\.\pipe\sakura_input_rival_instance_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        );
        let descriptor = Descriptor::for_pipe().expect("production descriptor");
        let _server = PipeInstance::create(&name, &descriptor, true).expect("first instance");
        let low_handle = low_integrity_non_appcontainer_token().expect("low token");
        let low_process_token = ProcessToken { handle: low_handle };
        assert_eq!(
            classify_token(&low_process_token),
            Ok(ClientTrust::LowIntegrity)
        );
        // Keep ownership in the explicit guard below after the classification
        // helper has borrowed the token.
        core::mem::forget(low_process_token);
        let low_token = HandleGuard(low_handle);
        // SAFETY: `low_token` owns a valid impersonation token for the duration
        // of the test; `RevertToSelfGuard` restores the thread token below.
        unsafe { ImpersonateLoggedOnUser(low_token.0).expect("impersonate low token") };
        let _revert = RevertToSelfGuard;

        let wide = to_wide_nul(&name);
        // SAFETY: `wide` is a live NUL-terminated pipe name. The returned
        // handle is checked and closed below.
        let rival = unsafe {
            CreateNamedPipeW(
                PCWSTR(wide.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                crate::transport::MAX_INSTANCES,
                8 * 1024,
                8 * 1024,
                0,
                None,
            )
        };
        assert_ne!(
            rival, INVALID_HANDLE_VALUE,
            "the test machine no longer reproduces the documented ACL property"
        );
        // SAFETY: this handle was returned by CreateNamedPipeW and is owned by
        // this test until it is explicitly closed.
        // SAFETY: this handle was returned by CreateNamedPipeW and is owned by
        // this test until it is explicitly closed.
        unsafe {
            let _ = CloseHandle(rival);
        }
    }

    fn low_integrity_non_appcontainer_token() -> windows::core::Result<HANDLE> {
        let mut current = HANDLE::default();
        // SAFETY: `current` is an out parameter for one owned process-token
        // handle, which is closed after duplication below.
        unsafe {
            OpenProcessToken(
                GetCurrentProcess(),
                TOKEN_DUPLICATE | TOKEN_QUERY,
                &mut current,
            )?;
        }
        let mut low = HANDLE::default();
        // SAFETY: `current` is a valid process token and `low` is a live out
        // parameter for one owned impersonation-token handle.
        let duplicated = unsafe {
            DuplicateTokenEx(
                current,
                TOKEN_ADJUST_DEFAULT | TOKEN_DUPLICATE | TOKEN_IMPERSONATE | TOKEN_QUERY,
                None,
                SecurityImpersonation,
                TokenImpersonation,
                &mut low,
            )
        };
        // SAFETY: `current` was returned by OpenProcessToken and is closed
        // exactly once here.
        unsafe {
            let _ = CloseHandle(current);
        }
        duplicated?;

        let mut is_appcontainer = 0u32;
        let mut needed = 0u32;
        // SAFETY: `low` is a valid token and both output buffers are live and
        // correctly sized for TokenIsAppContainer.
        unsafe {
            GetTokenInformation(
                low,
                TokenIsAppContainer,
                Some((&mut is_appcontainer as *mut u32).cast()),
                size_of::<u32>() as u32,
                &mut needed,
            )?;
        }
        if is_appcontainer != 0 {
            // SAFETY: the error branch still owns `low`, which is closed once.
            unsafe {
                let _ = CloseHandle(low);
            }
            return Err(windows::core::Error::new(
                windows::core::HRESULT(0x80004005u32 as i32),
                "the duplicated token is already an AppContainer",
            ));
        }

        let sid_text = to_wide_nul("S-1-16-4096");
        let mut sid = PSID::default();
        // SAFETY: `sid_text` is NUL terminated and `sid` receives LocalAlloc
        // storage that is freed after SetTokenInformation.
        unsafe { ConvertStringSidToSidW(PCWSTR(sid_text.as_ptr()), &mut sid)? };
        let label = TOKEN_MANDATORY_LABEL {
            Label: SID_AND_ATTRIBUTES {
                Sid: sid,
                Attributes: 0x20,
            },
        };
        // SAFETY: `low` is valid; `label` and its SID remain live for the call,
        // and the size includes the variable-length SID payload.
        let set_result = unsafe {
            SetTokenInformation(
                low,
                TokenIntegrityLevel,
                (&label as *const TOKEN_MANDATORY_LABEL).cast(),
                (size_of::<TOKEN_MANDATORY_LABEL>() + GetLengthSid(sid) as usize) as u32,
            )
        };
        // SAFETY: ConvertStringSidToSidW allocated `sid` with LocalAlloc; it is
        // freed exactly once after the consuming call returns.
        unsafe {
            let _ = LocalFree(Some(HLOCAL(sid.0)));
        }
        if let Err(error) = set_result {
            // SAFETY: this error branch still owns `low`, which is closed once.
            let _ = unsafe { CloseHandle(low) };
            return Err(error);
        }
        Ok(low)
    }

    #[test]
    fn a_refusal_names_the_step_that_refused() {
        // The current process is openable and its image path is readable, so
        // a policy naming something else can only fail at the comparison.
        // Issue #104 turns entirely on telling this apart from a path that
        // could not be read at all — both were `ERROR_ACCESS_DENIED` before.
        let elsewhere = std::env::temp_dir().join("not-the-engine.exe");
        assert_eq!(
            verify_server_process(std::process::id(), &ServerTrustPolicy::Exact(elsewhere)),
            Err(ServerRejection::ImagePathRejected)
        );

        // PID 0 is the idle process and can never be opened, so the refusal
        // has to be reported before any policy question is reached.
        match verify_server_process(0, &ServerTrustPolicy::Exact(PathBuf::from("x"))) {
            Err(ServerRejection::ProcessUnopenable(_)) => {}
            other => panic!("expected an unopenable process, got {other:?}"),
        }
    }

    #[test]
    fn every_refusal_prints_a_distinct_reason() {
        // A reason that reads like another reason is worth nothing in a CI
        // log, which is the only place Issue #104 is observable.
        let code = HRESULT::from_win32(windows::Win32::Foundation::ERROR_ACCESS_DENIED.0);
        let all = [
            ServerRejection::PolicyUnavailable,
            ServerRejection::NoServerProcessId,
            ServerRejection::ProcessUnopenable(code),
            ServerRejection::ImagePathUnreadable(code),
            ServerRejection::ImagePathRejected,
            ServerRejection::TokenUnopenable(code),
            ServerRejection::TokenUnclassifiable(code),
            ServerRejection::IntegrityRejected,
        ];
        let mut printed: Vec<String> = all.iter().map(ToString::to_string).collect();
        printed.sort();
        printed.dedup();
        assert_eq!(printed.len(), all.len(), "two reasons print the same text");
        for reason in all {
            assert!(!reason.to_string().is_empty());
        }
    }

    struct RevertToSelfGuard;

    impl Drop for RevertToSelfGuard {
        fn drop(&mut self) {
            // SAFETY: this guard exists only after successful impersonation and
            // restores the current thread before unwinding out of the test.
            unsafe {
                let _ = RevertToSelf();
            }
        }
    }

    struct HandleGuard(HANDLE);

    impl Drop for HandleGuard {
        fn drop(&mut self) {
            // SAFETY: the guard owns this handle and closes it exactly once.
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}
