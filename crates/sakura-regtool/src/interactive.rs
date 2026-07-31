//! "Is this token the person at the keyboard?"
//!
//! Per-user registration writes to HKCU and to a scheduled task owned by
//! the calling account. Both of those follow the *token*, not the desktop.
//! Under `runas /user:`, an SCCM/Intune deployment, or anything launched by
//! SYSTEM, the elevated process's HKCU is a different hive entirely — so
//! the IME gets enabled for an account nobody is sitting at, and every API
//! involved returns success while doing it (DESIGN 12.1).
//!
//! There is no single call that answers this. What we can establish
//! cheaply is two necessary conditions:
//!
//! 1. we are not in session 0, where no user is signed in at all, and
//! 2. the account we would write as is the account signed in to *this*
//!    session.
//!
//! Together those catch the deployment cases. They do not catch every
//! possible mismatch — a fully general answer needs `WTSQueryUserToken`,
//! which requires SYSTEM, and a tool that needed SYSTEM to check whether
//! it should be running as SYSTEM would be its own joke. What is left
//! uncovered is documented on [`Mismatch`].

use windows::core::{Error, PWSTR};
use windows::Win32::System::RemoteDesktop::{
    ProcessIdToSessionId, WTSDomainName, WTSFreeMemory, WTSQuerySessionInformationW, WTSUserName,
    WTS_CURRENT_SERVER_HANDLE, WTS_CURRENT_SESSION,
};
use windows::Win32::System::Threading::GetCurrentProcessId;

/// Why a per-user action was refused.
#[derive(Debug)]
pub enum Mismatch {
    /// Session 0 — a service or a deployment agent. There is no
    /// interactive user here to enable an IME for.
    NoInteractiveSession,
    /// The signed-in user and the calling token disagree. Reported with
    /// both names, because "it enabled for the wrong user" is unreadable
    /// without knowing which two accounts were involved.
    WrongAccount { session: String, token: String },
    /// The check itself failed. Treated as a refusal rather than as
    /// permission: an unverifiable identity is exactly the case this
    /// guard exists for.
    Undetermined(Error),
}

impl core::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoInteractiveSession => f.write_str(
                "this process is in session 0, which has no interactive user; \
                 run per-user registration in the user's own session",
            ),
            Self::WrongAccount { session, token } => write!(
                f,
                "the signed-in user is {session} but this process is running as \
                 {token}; per-user registration would enable Sakura Input for \
                 {token} instead",
            ),
            Self::Undetermined(error) => {
                write!(f, "could not determine the interactive user: {error}")
            }
        }
    }
}

/// Confirms that per-user state written now lands on the signed-in user.
///
/// Returns the account name on success, so a caller can name it in its own
/// output rather than resolving it a second time.
pub fn require_signed_in_user() -> Result<String, Mismatch> {
    if session_id().map_err(Mismatch::Undetermined)? == 0 {
        return Err(Mismatch::NoInteractiveSession);
    }

    let session = signed_in_account().map_err(Mismatch::Undetermined)?;
    let token = sakura_reg::launcher::current_account().map_err(Mismatch::Undetermined)?;

    // Windows account names are case-insensitive, and the two APIs do not
    // agree on casing: one reports what was typed at the logon prompt, the
    // other what SAM has stored.
    if session.eq_ignore_ascii_case(&token) {
        Ok(token)
    } else {
        Err(Mismatch::WrongAccount { session, token })
    }
}

fn session_id() -> windows::core::Result<u32> {
    let mut id = 0u32;
    // SAFETY: `id` is a valid out-parameter and the process id is our own.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut id) }?;
    Ok(id)
}

/// The account signed in to this session, as `DOMAIN\user` so it can be
/// compared with [`sakura_reg::launcher::current_account`] directly.
fn signed_in_account() -> windows::core::Result<String> {
    let user = session_string(WTSUserName)?;
    let domain = session_string(WTSDomainName)?;
    Ok(if domain.is_empty() {
        user
    } else {
        format!("{domain}\\{user}")
    })
}

fn session_string(
    class: windows::Win32::System::RemoteDesktop::WTS_INFO_CLASS,
) -> windows::core::Result<String> {
    let mut buffer = PWSTR::null();
    let mut bytes = 0u32;
    // SAFETY: both out-parameters are valid; the buffer WTS allocates is
    // freed below on every path, including the error path where WTS leaves
    // it null and `WTSFreeMemory(null)` is a no-op.
    unsafe {
        WTSQuerySessionInformationW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            WTS_CURRENT_SESSION,
            class,
            &mut buffer,
            &mut bytes,
        )?;
    }
    // SAFETY: on success WTS returns a NUL-terminated string.
    let value = unsafe { buffer.to_string() };
    // SAFETY: the buffer came from WTS and is not used again.
    unsafe { WTSFreeMemory(buffer.as_ptr().cast()) };
    Ok(value.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On a developer machine this runs in the signed-in user's own
    /// session, so the guard must pass and name that user. In CI it may
    /// legitimately run under a service account — the point of the
    /// assertion is that the answer is *decided*, not that it is yes.
    #[test]
    fn the_guard_reaches_a_verdict_rather_than_failing_to_look() {
        match require_signed_in_user() {
            Ok(account) => assert!(
                account.contains('\\'),
                "expected DOMAIN\\user, got {account:?}"
            ),
            Err(Mismatch::Undetermined(error)) => {
                panic!("the check itself broke: {error}")
            }
            Err(_) => {}
        }
    }
}
