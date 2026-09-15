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

mod admission;
mod process;
mod server_trust;
#[cfg(test)]
mod server_trust_tests;

pub use admission::{
    allow_sandbox_identity_queries, classify_client_process, pipe_name, pipe_name_for, sddl,
    sddl_for, ClientTrust, Descriptor, Endpoint, CLIENT_ACCESS,
};
pub use server_trust::{verify_server_process, ServerRejection, ServerTrustPolicy};

#[cfg(test)]
use admission::{
    classify_token, ALL_APPLICATION_PACKAGES, ALL_RESTRICTED_APPLICATION_PACKAGES, PIPE_PREFIX,
};
#[cfg(test)]
use process::{to_wide_nul, ProcessHandle, ProcessToken};
#[cfg(test)]
use server_trust::ENGINE_IMAGE_NAME;
#[cfg(test)]
use std::ffi::OsString;
#[cfg(test)]
use std::os::windows::ffi::{OsStrExt, OsStringExt};
#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use windows::core::HRESULT;
#[cfg(test)]
use windows::Win32::Security::SECURITY_ATTRIBUTES;
#[cfg(test)]
use windows::Win32::System::Threading::PROCESS_NAME_NATIVE;

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
