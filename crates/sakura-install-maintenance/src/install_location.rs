//! Install-location verification for administrator-only payloads (issue #360,
//! finding A1).
//!
//! The installer registers two things that elevated Windows components later
//! load or execute without asking again: a SYSTEM / highest-run-level logon
//! task that runs `{app}\sakura_regtool.exe`, and an HKLM COM registration
//! pointing at `versions\<ver>\sakura_tsf.dll`. Both are safe only while a
//! standard user cannot replace the file. "Inside Program Files" is the usual
//! way to get that, but it is a convention, not a property Windows enforces —
//! an administrator can type `C:\SakuraInput` into the directory page, and a
//! directory there inherits `Authenticated Users: Modify` from `C:\`.
//!
//! [`verify_admin_only_payload`] therefore checks the actual security
//! descriptors: the payload file itself, every directory from its parent up to
//! and including the install root (a non-admin writer on any of them can
//! replace or rename the payload into place), and every directory above the
//! install root up to the volume root (a `FILE_DELETE_CHILD` or `DELETE`
//! writer there can remove the whole subtree and re-create it).
//!
//! The verdict logic is split from the Win32 reading so it can be tested
//! against security descriptors built from SDDL strings; the reader opens one
//! handle per component with `FILE_FLAG_BACKUP_SEMANTICS |
//! FILE_FLAG_OPEN_REPARSE_POINT` and reads attributes, owner, and DACL from
//! that same handle, so the reparse check and the ACL read can never describe
//! two different objects.

use std::ffi::c_void;
use std::fmt;
use std::path::{Path, PathBuf};

use windows::core::{Error, Result, HRESULT, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, LocalFree, ERROR_SUCCESS, HANDLE, HLOCAL};
use windows::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows::Win32::Security::{
    GetAce, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_MODE, OPEN_EXISTING,
};

/// Local `SYSTEM` — service accounts and scheduled tasks.
const SID_SYSTEM: &str = "S-1-5-18";
/// Local `Administrators` — whoever installed the product.
const SID_ADMINISTRATORS: &str = "S-1-5-32-544";
/// `TrustedInstaller` — owns `C:\` and `C:\Program Files` on a stock
/// Windows 11 machine, so the ancestor walk must accept it.
const SID_TRUSTED_INSTALLER: &str =
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";

/// Owners and grantees Windows already stops a standard user from
/// impersonating. Anything outside this set is treated as user-writable.
const ALLOWLIST: &[&str] = &[SID_SYSTEM, SID_ADMINISTRATORS, SID_TRUSTED_INSTALLER];

/// `INHERIT_ONLY_ACE` (`ACE_HEADER.AceFlags` bit): the ACE exists only to be
/// propagated to children and cannot itself be used to modify this object.
const INHERIT_ONLY_ACE_FLAG: u8 = 0x08;

/// `ACE_HEADER.AceType` values this verifier understands. Only a plain
/// `ACCESS_ALLOWED_ACE` is judged on its mask; every other allow flavour is
/// refused for a non-allowlisted SID rather than second-guessed.
const ACE_ALLOWED: u8 = 0x00;
const ACE_DENIED: u8 = 0x01;
const ACE_ALLOWED_OBJECT: u8 = 0x05;
const ACE_DENIED_OBJECT: u8 = 0x06;
const ACE_ALLOWED_CALLBACK: u8 = 0x09;
const ACE_DENIED_CALLBACK: u8 = 0x0A;
const ACE_ALLOWED_CALLBACK_OBJECT: u8 = 0x0B;
const ACE_DENIED_CALLBACK_OBJECT: u8 = 0x0C;

/// `ACE_OBJECT_TYPE_PRESENT` / `ACE_INHERITED_OBJECT_TYPE_PRESENT`, from the
/// `Flags` field of an object ACE, which decides where its SID starts.
const OBJECT_TYPE_PRESENT: u32 = 0x01;
const INHERITED_OBJECT_TYPE_PRESENT: u32 = 0x02;

const FILE_WRITE_DATA: u32 = 0x0000_0002;
const FILE_APPEND_DATA: u32 = 0x0000_0004;
const FILE_DELETE_CHILD: u32 = 0x0000_0040;
const DELETE: u32 = 0x0001_0000;
const WRITE_DAC: u32 = 0x0004_0000;
const WRITE_OWNER: u32 = 0x0008_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const GENERIC_ALL: u32 = 0x1000_0000;
const MAXIMUM_ALLOWED: u32 = 0x0200_0000;

/// Rights that let a writer replace the payload itself or plant a sibling
/// (a rogue `sakura_tsf.dll` dropped next to a registered one is the
/// DLL-planting half of A1, so `ADD_FILE`/`ADD_SUBDIRECTORY` count here).
const DANGEROUS_INSTALL: u32 = FILE_WRITE_DATA
    | FILE_APPEND_DATA
    | FILE_DELETE_CHILD
    | DELETE
    | WRITE_DAC
    | WRITE_OWNER
    | GENERIC_WRITE
    | GENERIC_ALL
    | MAXIMUM_ALLOWED;

/// Rights that let a writer remove the install subtree from an ancestor.
/// Deliberately excludes `FILE_APPEND_DATA` (a directory's add-subdirectory):
/// a stock `C:\` grants Authenticated Users `LC`, which can create a *new*
/// child but cannot replace the existing install directory, so refusing it
/// would reject the supported `C:\Program Files\...` layout.
const DANGEROUS_ANCESTOR: u32 =
    FILE_DELETE_CHILD | DELETE | WRITE_DAC | WRITE_OWNER | GENERIC_ALL | MAXIMUM_ALLOWED;

/// `READ_CONTROL | FILE_READ_ATTRIBUTES`: enough to read the descriptor and
/// the attributes, never more.
const READ_ACCESS: u32 = 0x0002_0080;
/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE` — the inspection
/// must not lock an installer or uninstaller out of the file.
const OPEN_SHARE: u32 = 0x0000_0007;
/// `FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT`.
const OPEN_FLAGS: u32 = 0x0220_0000;

/// Which half of the component walk a path belongs to; it selects the
/// dangerous mask, nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentKind {
    /// The payload file or a directory inside/including the install root.
    Install,
    /// A directory strictly above the install root, up to the volume root.
    Ancestor,
}

/// One DACL entry reduced to what the verdict needs. `ace_type` and `flags`
/// are the raw `ACE_HEADER` bytes so fail-closed handling of unknown ACE
/// flavours (object, callback) stays in the evaluator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AceEntry {
    pub ace_type: u8,
    pub flags: u8,
    pub mask: u32,
    /// SDDL-form SID string (e.g. `S-1-5-18`), which keeps the evaluator
    /// independent of SID buffer lifetimes. Empty when the SID could not be
    /// located — still a rejection, because empty is never allowlisted.
    pub sid: String,
}

/// Why a single component or the payload/root relation failed verification.
/// Paired with the offending path in [`LocationError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// The payload is not inside the install root (canonical-path compare).
    OutsideInstallRoot,
    /// A reparse point (junction, symlink, mounted folder) means the path the
    /// ACL describes is not the path that will be executed.
    ReparsePoint,
    /// The owner is neither SYSTEM, Administrators, nor TrustedInstaller. A
    /// user-owned object can have its DACL rewritten by that user regardless
    /// of what the DACL currently says.
    UntrustedOwner(String),
    /// A NULL DACL grants everyone everything.
    NullDacl,
    /// A non-allowlisted SID holds a dangerous right, or arrives through an
    /// ACE flavour this verifier does not understand (fail closed).
    UnsafeAce {
        ace_type: u8,
        sid: String,
        mask: u32,
    },
    /// The component could not be opened or its security could not be read.
    /// An uninspectable path must never pass.
    Unreadable(String),
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Rejection::OutsideInstallRoot => {
                write!(f, "is outside the install root")
            }
            Rejection::ReparsePoint => write!(f, "is a reparse point"),
            Rejection::UntrustedOwner(sid) => {
                write!(
                    f,
                    "is owned by {sid}, not SYSTEM/Administrators/TrustedInstaller"
                )
            }
            Rejection::NullDacl => {
                write!(f, "has a NULL DACL (grants everyone full access)")
            }
            Rejection::UnsafeAce {
                ace_type,
                sid,
                mask,
            } => write!(
                f,
                "grants {sid} a dangerous or unrecognized right \
                 (ace type {ace_type}, mask 0x{mask:08x})"
            ),
            Rejection::Unreadable(why) => {
                write!(f, "could not be inspected: {why}")
            }
        }
    }
}

/// A verification failure naming the offending path and the reason, so the
/// installer log points at exactly which directory or file is unsafe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationError {
    pub path: PathBuf,
    pub rejection: Rejection,
}

impl fmt::Display for LocationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "install location is not administrator-only: {} {}",
            self.path.display(),
            self.rejection
        )
    }
}

impl std::error::Error for LocationError {}

/// The pure verdict for one inspected component. `dacl` is `None` for a NULL
/// DACL; order is irrelevant because any single unsafe grant is a rejection.
fn evaluate_component(
    owner: &str,
    dacl: Option<&[AceEntry]>,
    kind: ComponentKind,
) -> std::result::Result<(), Rejection> {
    if !ALLOWLIST.contains(&owner) {
        return Err(Rejection::UntrustedOwner(owner.to_owned()));
    }
    let aces = dacl.ok_or(Rejection::NullDacl)?;
    let dangerous = match kind {
        ComponentKind::Install => DANGEROUS_INSTALL,
        ComponentKind::Ancestor => DANGEROUS_ANCESTOR,
    };
    for ace in aces {
        if ace.flags & INHERIT_ONLY_ACE_FLAG != 0 {
            continue;
        }
        if matches!(
            ace.ace_type,
            ACE_DENIED | ACE_DENIED_OBJECT | ACE_DENIED_CALLBACK | ACE_DENIED_CALLBACK_OBJECT
        ) {
            continue;
        }
        if ALLOWLIST.contains(&ace.sid.as_str()) {
            continue;
        }
        // Only a plain ACCESS_ALLOWED_ACE is reasoned about. Object, callback,
        // and any newer allow flavour from a non-allowlisted SID fails closed.
        if ace.ace_type != ACE_ALLOWED || ace.mask & dangerous != 0 {
            return Err(Rejection::UnsafeAce {
                ace_type: ace.ace_type,
                sid: ace.sid.clone(),
                mask: ace.mask,
            });
        }
    }
    Ok(())
}

/// Verifies that `payload`, which must sit inside `install_root`, can only be
/// replaced by administrators. See the module documentation for the threat
/// this blocks.
pub fn verify_admin_only_payload(
    install_root: &Path,
    payload: &Path,
) -> std::result::Result<(), LocationError> {
    // Containment is decided on canonical paths so `..`, short names, and
    // case differences cannot smuggle a payload outside the root.
    let canonical_root = canonical(install_root);
    let canonical_payload = canonical(payload);
    if !canonical_payload.starts_with(&canonical_root) || canonical_payload == canonical_root {
        return Err(LocationError {
            path: payload.to_path_buf(),
            rejection: Rejection::OutsideInstallRoot,
        });
    }

    // The component walk, however, uses the literal path: canonicalizing
    // resolves junctions, which would silently remove the very reparse points
    // this check exists to reject. The root boundary is still recognized
    // canonically so the install/ancestor classification stays correct even
    // when the caller's spelling differs in case.
    let literal_root = absolute(install_root);
    let literal_payload = absolute(payload);

    let mut components: Vec<(PathBuf, ComponentKind)> =
        vec![(literal_payload.clone(), ComponentKind::Install)];
    let mut reached_root = false;
    if let Some(mut dir) = literal_payload.parent() {
        loop {
            components.push((
                dir.to_path_buf(),
                if reached_root {
                    ComponentKind::Ancestor
                } else {
                    ComponentKind::Install
                },
            ));
            if !reached_root && (same_dir(dir, &literal_root) || canonical(dir) == canonical_root) {
                reached_root = true;
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => break,
            }
        }
    }

    // Two passes over one inspection per component: a reparse point anywhere
    // in the chain is rejected before any ACL verdict, so a junction cannot
    // hide behind an earlier directory's otherwise-acceptable descriptor.
    let mut inspected = Vec::with_capacity(components.len());
    for (path, kind) in components {
        let inspection = inspect(&path).map_err(|error| LocationError {
            path: path.clone(),
            rejection: Rejection::Unreadable(error.to_string()),
        })?;
        inspected.push((path, kind, inspection));
    }
    for (path, _, inspection) in &inspected {
        if matches!(inspection, Inspection::ReparsePoint) {
            return Err(LocationError {
                path: path.clone(),
                rejection: Rejection::ReparsePoint,
            });
        }
    }
    for (path, kind, inspection) in inspected {
        let Inspection::Descriptor { owner, dacl } = inspection else {
            unreachable!("reparse points were rejected above");
        };
        evaluate_component(&owner, dacl.as_deref(), kind)
            .map_err(|rejection| LocationError { path, rejection })?;
    }
    Ok(())
}

/// Canonicalizes for containment checks only. A failure keeps the literal
/// path; the walk then fails at an unreadable or outside component rather
/// than mistaking an unresolvable path for a verdict.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Absolutizes without resolving links — `canonicalize` must not be used for
/// the walked chain because it would erase reparse points.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Case-insensitive directory equality on normalized spellings; Windows file
/// names are case-insensitive and a trailing separator is meaningless.
fn same_dir(left: &Path, right: &Path) -> bool {
    fn key(path: &Path) -> String {
        let mut text = path.to_string_lossy().to_lowercase();
        while text.ends_with('\\') || text.ends_with('/') {
            text.pop();
        }
        text
    }
    key(left) == key(right)
}

enum Inspection {
    ReparsePoint,
    Descriptor {
        owner: String,
        dacl: Option<Vec<AceEntry>>,
    },
}

/// Reads attributes, owner SID, and DACL for `path` from a single handle, so
/// the reparse check and the ACL read can never describe two objects.
fn inspect(path: &Path) -> Result<Inspection> {
    let mut wide = sakura_reg::wide::os_to_wide(path.as_os_str());
    wide.push(0);
    // SAFETY: `wide` is NUL-terminated and outlives the call. Access is the
    // minimal read set; BACKUP_SEMANTICS lets this open directories and
    // OPEN_REPARSE_POINT makes the descriptor describe the link itself, not
    // its target.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            READ_ACCESS,
            FILE_SHARE_MODE(OPEN_SHARE),
            None,
            OPEN_EXISTING,
            FILE_FLAGS_AND_ATTRIBUTES(OPEN_FLAGS),
            None,
        )?
    };
    let handle = HandleGuard(handle);

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `handle` is live and `info` is a valid output.
    unsafe { GetFileInformationByHandle(handle.0, &mut info) }?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT.0 != 0 {
        return Ok(Inspection::ReparsePoint);
    }

    let mut owner = PSID::default();
    let mut dacl: *mut ACL = core::ptr::null_mut();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `handle` is live; all outputs are valid writable pointers and
    // `descriptor` owns the returned allocation, released below.
    let status = unsafe {
        GetSecurityInfo(
            handle.0,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            Some(&mut dacl),
            None,
            Some(&mut descriptor),
        )
    };
    if status != ERROR_SUCCESS {
        return Err(Error::from_hresult(HRESULT::from_win32(status.0)));
    }
    let _descriptor = LocalAllocation(HLOCAL(descriptor.0));

    let owner = sid_string(owner)?;
    let dacl = if dacl.is_null() {
        None
    } else {
        Some(collect_dacl(dacl)?)
    };
    Ok(Inspection::Descriptor { owner, dacl })
}

/// Reduces a DACL to [`AceEntry`]s in ACL order. Mask and SID are extracted
/// only where the ACE layout is known; unrecognized flavours keep their type
/// byte so [`evaluate_component`] can fail closed on them.
fn collect_dacl(dacl: *const ACL) -> Result<Vec<AceEntry>> {
    // SAFETY: `dacl` points at a live ACL obtained from a security descriptor
    // that outlives this call; each ACE pointer GetAce returns is borrowed
    // from that same ACL.
    unsafe {
        let count = (*dacl).AceCount as u32;
        let mut aces = Vec::with_capacity(count as usize);
        for index in 0..count {
            let mut ace: *mut c_void = core::ptr::null_mut();
            GetAce(dacl, index, &mut ace)?;
            let header = &*(ace as *const ACE_HEADER);
            // Every ACE layout places ACCESS_MASK at offset 4, so the mask is
            // always readable even when the SID is not.
            let mask = *(ace as *const u32).add(1);
            let sid = sid_at(ace, header.AceType)?.unwrap_or_default();
            aces.push(AceEntry {
                ace_type: header.AceType,
                flags: header.AceFlags,
                mask,
                sid,
            });
        }
        Ok(aces)
    }
}

/// Locates the SID inside an ACE whose layout is known. Returns `None` for a
/// layout this verifier does not parse; the entry then carries an empty SID,
/// which is never allowlisted and therefore fails closed.
fn sid_at(ace: *const c_void, ace_type: u8) -> Result<Option<String>> {
    // SAFETY: `ace` points at a live ACE inside a DACL that outlives this
    // call; the layouts read below are the documented ACE structures for the
    // matched type values.
    unsafe {
        let sid_ptr = match ace_type {
            // ACCESS_ALLOWED_ACE / ACCESS_DENIED_ACE and their plain callback
            // forms share `Header, Mask, SidStart`.
            ACE_ALLOWED | ACE_DENIED | ACE_ALLOWED_CALLBACK | ACE_DENIED_CALLBACK => {
                std::ptr::addr_of!((*(ace as *const ACCESS_ALLOWED_ACE)).SidStart)
            }
            // The object forms insert a flags field and up to two GUIDs
            // before the SID.
            ACE_ALLOWED_OBJECT
            | ACE_DENIED_OBJECT
            | ACE_ALLOWED_CALLBACK_OBJECT
            | ACE_DENIED_CALLBACK_OBJECT => {
                let base = ace as *const u8;
                let flags = *(base.add(8) as *const u32);
                let mut offset = 12usize;
                if flags & OBJECT_TYPE_PRESENT != 0 {
                    offset += 16;
                }
                if flags & INHERITED_OBJECT_TYPE_PRESENT != 0 {
                    offset += 16;
                }
                return sid_string(PSID(base.add(offset) as *mut c_void)).map(Some);
            }
            _ => return Ok(None),
        };
        sid_string(PSID(sid_ptr as *mut c_void)).map(Some)
    }
}

/// Renders a kernel SID as an SDDL string for comparison and diagnostics.
fn sid_string(sid: PSID) -> Result<String> {
    let mut text = PWSTR::null();
    // SAFETY: `sid` points at a valid SID obtained from a live security
    // descriptor; `text` receives a LocalAlloc string freed below.
    unsafe {
        windows::Win32::Security::Authorization::ConvertSidToStringSidW(sid, &mut text)?;
    }
    // SAFETY: `text` is a valid NUL-terminated LocalAlloc string on success.
    let value = unsafe { text.to_string() }?;
    // SAFETY: `text` was allocated by ConvertSidToStringSidW via LocalAlloc.
    unsafe {
        let _ = LocalFree(Some(HLOCAL(text.0.cast())));
    }
    Ok(value)
}

/// Closes a file handle on drop so the inspection cannot leak one.
struct HandleGuard(HANDLE);

impl Drop for HandleGuard {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a live handle obtained from CreateFileW and is
        // closed exactly once here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Frees a `LocalAlloc` allocation on drop.
struct LocalAllocation(HLOCAL);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a LocalAlloc allocation owned by this guard.
        unsafe {
            let _ = LocalFree(Some(self.0));
        }
    }
}

#[cfg(test)]
mod tests {
    //! Contract coverage C3–C8 and C11. Expected verdicts are written from the
    //! task contracts and Windows ACL semantics — never from what the
    //! evaluator happens to return.

    use super::*;

    use windows::core::BOOL;
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::{GetSecurityDescriptorDacl, GetSecurityDescriptorOwner};

    /// Builds (owner SID string, DACL entries) from an SDDL string, the same
    /// reduction the production reader performs on `GetSecurityInfo` output.
    fn descriptor(sddl: &str) -> (String, Option<Vec<AceEntry>>) {
        let wide = sakura_reg::wide::to_wide_nul(sddl);
        let mut sd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `wide` is NUL-terminated and outlives the call; `sd` is a
        // valid out-parameter receiving a LocalAlloc descriptor freed below.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wide.as_ptr()),
                SDDL_REVISION_1,
                &mut sd,
                None,
            )
            .unwrap_or_else(|error| panic!("SDDL {sddl:?} did not parse: {error}"));
        }
        let _sd = LocalAllocation(HLOCAL(sd.0));

        let mut owner = PSID::default();
        let mut defaulted = BOOL::default();
        // SAFETY: `sd` is a live descriptor; both outputs are valid pointers.
        unsafe {
            GetSecurityDescriptorOwner(sd, &mut owner, &mut defaulted).expect("owner extraction");
        }
        let owner = sid_string(owner).expect("owner SID string");

        let mut present = BOOL::default();
        let mut acl: *mut ACL = core::ptr::null_mut();
        let mut acl_defaulted = BOOL::default();
        // SAFETY: `sd` is a live descriptor; all outputs are valid pointers.
        unsafe {
            GetSecurityDescriptorDacl(sd, &mut present, &mut acl, &mut acl_defaulted)
                .expect("DACL extraction");
        }
        let dacl = if !present.as_bool() || acl.is_null() {
            None
        } else {
            Some(collect_dacl(acl).expect("ACE collection"))
        };
        (owner, dacl)
    }

    /// The real `C:\` descriptor observed on this machine: TrustedInstaller
    /// owner; AU `LC` (non inherit-only) plus an inherit-only `SDGXGWGR`.
    const SDDL_C_ROOT: &str = "O:S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464D:(A;;LC;;;AU)(A;OICIIO;SDGXGWGR;;;AU)(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)";

    /// The real `C:\Program Files` descriptor (condensed to the decisive
    /// entries): CO/SY/BA inherit-only GA, BU/AC read, TI full.
    const SDDL_PROGRAM_FILES: &str = "O:S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464D:PAI(A;OICIIO;GA;;;CO)(A;;0x1301bf;;;SY)(A;;0x1301bf;;;BA)(A;OICIIO;GXGR;;;BU)(A;;0x1200a9;;;BU)(A;;FA;;;S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464)(A;;0x1200a9;;;AC)(A;;0x1200a9;;;S-1-15-2-2)";

    /// The real `C:\Program Files\Sakura Input` descriptor.
    const SDDL_INSTALL_ROOT: &str = "O:BA D:AI(A;ID;FA;;;S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464)(A;CIIOID;GA;;;S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464)(A;ID;FA;;;SY)(A;OICIIOID;GA;;;SY)(A;ID;FA;;;BA)(A;OICIIOID;GA;;;BA)(A;ID;0x1200a9;;;BU)(A;OICIIOID;GXGR;;;BU)(A;OICIIOID;GA;;;CO)(A;ID;0x1200a9;;;AC)(A;ID;0x1200a9;;;S-1-15-2-2)";

    /// The real payload file descriptor (regtool.exe / sakura_tsf.dll).
    const SDDL_PAYLOAD_FILE: &str =
        "O:BA D:AI(A;ID;FA;;;SY)(A;ID;FA;;;BA)(A;ID;0x1200a9;;;BU)(A;ID;0x1200a9;;;AC)(A;ID;0x1200a9;;;S-1-15-2-2)";

    /// C8 (liveness): the reference descriptors for the supported Program
    /// Files layout all pass at the component kind they occupy in the walk.
    #[test]
    fn c8_the_real_program_files_layout_passes() {
        let (owner, dacl) = descriptor(SDDL_PAYLOAD_FILE);
        assert_eq!(
            evaluate_component(&owner, dacl.as_deref(), ComponentKind::Install),
            Ok(())
        );

        let (owner, dacl) = descriptor(SDDL_INSTALL_ROOT);
        assert_eq!(
            evaluate_component(&owner, dacl.as_deref(), ComponentKind::Install),
            Ok(())
        );

        let (owner, dacl) = descriptor(SDDL_PROGRAM_FILES);
        assert_eq!(
            evaluate_component(&owner, dacl.as_deref(), ComponentKind::Ancestor),
            Ok(())
        );

        let (owner, dacl) = descriptor(SDDL_C_ROOT);
        assert_eq!(
            evaluate_component(&owner, dacl.as_deref(), ComponentKind::Ancestor),
            Ok(())
        );
    }

    /// C5: a directory created under `C:\` inherits this Authenticated Users
    /// grant (`SD` includes DELETE); as an install component it must fail.
    #[test]
    fn c5_authenticated_users_modify_on_the_install_root_is_rejected() {
        let (owner, dacl) = descriptor(
            "O:BA D:AI(A;ID;FA;;;SY)(A;ID;FA;;;BA)(A;OICIID;SDGXGWGR;;;AU)(A;ID;0x1200a9;;;BU)",
        );
        let error = evaluate_component(&owner, dacl.as_deref(), ComponentKind::Install)
            .expect_err("AU modify on an install component must fail");
        let Rejection::UnsafeAce { sid, mask, .. } = error else {
            panic!("expected an unsafe-ACE rejection, got {error:?}");
        };
        assert_eq!(sid, "S-1-5-11");
        assert_ne!(mask & DANGEROUS_INSTALL, 0);
    }

    /// C3: a user-owned install root is refused even when every ACE is
    /// administrator-shaped, because that user can rewrite the DACL.
    #[test]
    fn c3_a_user_owned_component_is_rejected() {
        let (owner, dacl) = descriptor(
            "O:S-1-5-21-1004336348-1177238915-682003330-1001D:AI(A;ID;FA;;;SY)(A;ID;FA;;;BA)",
        );
        assert_eq!(
            evaluate_component(&owner, dacl.as_deref(), ComponentKind::Install),
            Err(Rejection::UntrustedOwner(
                "S-1-5-21-1004336348-1177238915-682003330-1001".to_owned()
            ))
        );
    }

    /// C4.
    #[test]
    fn c4_a_null_dacl_is_rejected() {
        assert_eq!(
            evaluate_component("S-1-5-18", None, ComponentKind::Install),
            Err(Rejection::NullDacl)
        );
        assert_eq!(
            evaluate_component("S-1-5-18", None, ComponentKind::Ancestor),
            Err(Rejection::NullDacl)
        );
    }

    /// C5 second half: an inherit-only AU grant cannot modify the component
    /// itself, so it does not reject.
    #[test]
    fn c5_an_inherit_only_grant_is_ignored() {
        let (owner, dacl) = descriptor("O:BA D:(A;OICIIO;SDGXGWGR;;;AU)(A;;FA;;;BA)(A;;FA;;;SY)");
        assert_eq!(
            evaluate_component(&owner, dacl.as_deref(), ComponentKind::Install),
            Ok(())
        );
    }

    /// C6 first half.
    #[test]
    fn c6_an_ancestor_with_only_add_subdirectory_for_users_passes() {
        let (owner, dacl) = descriptor("O:S-1-5-18D:(A;;LC;;;AU)(A;;FA;;;BA)");
        assert_eq!(
            evaluate_component(&owner, dacl.as_deref(), ComponentKind::Ancestor),
            Ok(())
        );
    }

    /// C6 second half: delete-child on an ancestor can remove the whole
    /// install subtree.
    #[test]
    fn c6_an_ancestor_with_delete_child_for_users_is_rejected() {
        let (owner, dacl) = descriptor("O:S-1-5-18D:(A;;0x40;;;AU)(A;;FA;;;BA)");
        let error = evaluate_component(&owner, dacl.as_deref(), ComponentKind::Ancestor)
            .expect_err("AU delete-child on an ancestor must fail");
        assert!(matches!(error, Rejection::UnsafeAce { .. }), "{error:?}");
    }

    /// C6 third half: standard DELETE on the ancestor is just as fatal.
    #[test]
    fn c6_an_ancestor_with_delete_for_users_is_rejected() {
        let (owner, dacl) = descriptor("O:S-1-5-18D:(A;;SD;;;AU)(A;;FA;;;BA)");
        let error = evaluate_component(&owner, dacl.as_deref(), ComponentKind::Ancestor)
            .expect_err("AU delete on an ancestor must fail");
        assert!(matches!(error, Rejection::UnsafeAce { .. }), "{error:?}");
    }

    /// C7: object and callback ACE flavours from a non-allowlisted SID are
    /// not reasoned about; they fail closed.
    #[test]
    fn c7_unknown_ace_flavours_fail_closed() {
        for ace_type in [
            ACE_ALLOWED_OBJECT,
            ACE_ALLOWED_CALLBACK,
            ACE_ALLOWED_CALLBACK_OBJECT,
            0xF0, // a type Windows does not define
        ] {
            let aces = [AceEntry {
                ace_type,
                flags: 0,
                mask: 0,
                sid: "S-1-5-32-545".to_owned(),
            }];
            let error = evaluate_component("S-1-5-18", Some(&aces), ComponentKind::Install)
                .expect_err("a non-plain ACE from a user SID must fail closed");
            assert!(matches!(error, Rejection::UnsafeAce { .. }), "{error:?}");
        }
    }

    /// C5 companion / C6 boundary: add-file on an *install* directory enables
    /// DLL planting, while the identical grant on a pure ancestor cannot
    /// replace an existing child.
    #[test]
    fn the_install_and_ancestor_masks_differ_on_add_file() {
        let aces = [AceEntry {
            ace_type: ACE_ALLOWED,
            flags: 0,
            mask: FILE_WRITE_DATA,
            sid: "S-1-5-32-545".to_owned(),
        }];
        let error = evaluate_component("S-1-5-18", Some(&aces), ComponentKind::Install)
            .expect_err("AU add-file on an install directory must fail");
        assert!(matches!(error, Rejection::UnsafeAce { .. }), "{error:?}");

        assert_eq!(
            evaluate_component("S-1-5-18", Some(&aces), ComponentKind::Ancestor),
            Ok(())
        );
    }

    /// C11: a rejection names the path and the reason.
    #[test]
    fn c11_the_error_names_the_path_and_reason() {
        let error = LocationError {
            path: PathBuf::from(r"C:\SakuraInput"),
            rejection: Rejection::UntrustedOwner("S-1-5-21-1-2-3-1001".to_owned()),
        };
        let text = error.to_string();
        assert!(text.contains("C:\\SakuraInput"), "{text}");
        assert!(text.contains("S-1-5-21-1-2-3-1001"), "{text}");
    }
}
