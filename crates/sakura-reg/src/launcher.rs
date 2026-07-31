//! The per-user logon task that starts Sakura Input.
//!
//! The engine is resident, not on demand (DESIGN 4.3): it starts once at
//! logon and lives until the session ends, so a 150 ms cold start can
//! never land inside a 50 ms keystroke budget. Something has to start it,
//! and that something cannot be the DLL — a DLL instance loaded into an
//! AppContainer or low-IL host cannot reliably create processes or
//! activate the Task Scheduler's COM server, and a text service that
//! spawns processes from inside a sandboxed browser tab would be a
//! design defect even if it worked. So the operating system starts it,
//! from a task registered here.
//!
//! # Why the settings below are not decoration
//!
//! A task created with default settings would look correct in the Task
//! Scheduler UI and still fail as an IME launcher, in ways that surface
//! days later and look like the IME crashed:
//!
//! - `ExecutionTimeLimit` defaults to three days, after which the task's
//!   process is **terminated**. An IME that stops working after 72 hours
//!   of uptime is the most expensive kind of bug to diagnose, because
//!   nobody connects it to a scheduled task.
//! - `DisallowStartIfOnBatteries` defaults to *true*. On a laptop that is
//!   simply "the IME does not start", which is most laptops.
//! - `StopIfGoingOnBatteries` defaults to *true*: unplug the machine and
//!   the IME dies mid-sentence.
//! - `StopOnIdleEnd` defaults to *true*, which stops the task when the
//!   machine stops being idle — i.e. when the user comes back and starts
//!   typing.
//!
//! Each of those is a default that suits a nightly maintenance job and is
//! wrong for a resident user-facing service.
//!
//! # Integrity level
//!
//! The task runs at the user's own level ([`TASK_RUNLEVEL_LUA`]), never
//! elevated. An elevated engine would hold every keystroke the user types
//! at high integrity for no benefit, and UIPI would then stop the
//! normal-integrity renderer from talking to it.
//!
//! # Safety
//!
//! Every `unsafe` block in this file is a call through a Task Scheduler
//! COM interface. The obligation is the same each time and is discharged
//! the same way: the interface came from a `Result` this code already
//! unwrapped, so the pointer is live and the apartment it was created in
//! is still initialized (the caller holds a [`crate::ComApartment`]); and
//! every `BSTR`/`VARIANT` argument is a temporary that outlives the call
//! it is passed to. The per-site comments below add only what is specific
//! to that site.

use std::path::Path;

use windows::Win32::Foundation::{ERROR_MORE_DATA, E_INVALIDARG, VARIANT_FALSE, VARIANT_TRUE};
use windows::Win32::Security::Authentication::Identity::{GetUserNameExW, NameSamCompatible};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::TaskScheduler::{
    IExecAction, ILogonTrigger, ITaskFolder, ITaskService, TaskScheduler, TASK_ACTION_EXEC,
    TASK_CREATE_OR_UPDATE, TASK_ENUM_HIDDEN, TASK_INSTANCES_IGNORE_NEW,
    TASK_LOGON_INTERACTIVE_TOKEN, TASK_RUNLEVEL_LUA, TASK_TRIGGER_LOGON,
};
use windows::Win32::System::Variant::VARIANT;
use windows_core::{Error, Interface, Result, BSTR, HRESULT, PWSTR};

/// The folder the task is filed under, when the user is allowed to make
/// one.
const FOLDER: &str = "Sakura Input";

/// What the task is called inside [`FOLDER`].
const TASK_LEAF: &str = "Logon";

/// What the task is called when it has to live in the root folder
/// instead, where it shares a namespace with everything else on the
/// machine.
const TASK_ROOT_PREFIX: &str = "Sakura Input Logon";

/// `HRESULT_FROM_WIN32(ERROR_ALREADY_EXISTS)`, which is how
/// `ITaskFolder::CreateFolder` reports a folder we already made.
const ALREADY_EXISTS: HRESULT = HRESULT(0x8007_00B7u32 as i32);

/// `HRESULT_FROM_WIN32(ERROR_ACCESS_DENIED)`.
const ACCESS_DENIED: HRESULT = HRESULT(0x8007_0005u32 as i32);

/// `HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)`, which the Task Scheduler
/// returns for both a missing folder and a missing task.
const NOT_FOUND: HRESULT = HRESULT(0x8007_0002u32 as i32);

/// Where the task lives, and what it is called there.
///
/// Two possibilities rather than one because a standard user may create a
/// task but may not always create a folder to put it in. Falling back to
/// the root folder keeps the IME working for that user; refusing to
/// register unless we get our own folder would trade their IME for our
/// tidiness.
struct Location {
    folder: ITaskFolder,
    name: String,
    /// The root folder, kept so that an emptied [`FOLDER`] can be removed.
    root: ITaskFolder,
    /// Whether [`folder`](Self::folder) is our own subfolder rather than
    /// the root.
    scoped: bool,
}

impl Location {
    /// Resolves the folder, creating it if allowed, and derives a task
    /// name that is unique to this account.
    ///
    /// `create` distinguishes registration (make the folder) from removal
    /// and inspection (find it or give up), so that asking whether the
    /// task exists never has the side effect of creating a folder.
    fn resolve(service: &ITaskService, account: &str, create: bool) -> Result<Self> {
        // SAFETY: `service` is connected, and "\" is the root folder, which
        // exists on every machine.
        let root = unsafe { service.GetFolder(&BSTR::from("\\")) }?;

        let scoped = if create {
            // SAFETY: `root` is live; the empty VARIANT is "default
            // security", i.e. inherit from the parent folder.
            match unsafe { root.CreateFolder(&BSTR::from(FOLDER), &VARIANT::default()) } {
                Ok(folder) => Some(folder),
                Err(error) if error.code() == ALREADY_EXISTS => {
                    // SAFETY: as above; the folder is known to exist,
                    // because that is what the error said.
                    Some(unsafe { root.GetFolder(&BSTR::from(FOLDER)) }?)
                }
                Err(error) if error.code() == ACCESS_DENIED => None,
                Err(error) => return Err(error),
            }
        } else {
            // SAFETY: as above. A missing folder is an `Err`, not a fault.
            unsafe { root.GetFolder(&BSTR::from(FOLDER)) }.ok()
        };

        Ok(match scoped {
            Some(folder) => Location {
                folder,
                name: TASK_LEAF.to_owned(),
                root,
                scoped: true,
            },
            None => Location {
                folder: root.clone(),
                name: format!("{TASK_ROOT_PREFIX} ({})", sanitize(account)),
                root,
                scoped: false,
            },
        })
    }

    /// Removes our folder once nothing is left in it.
    ///
    /// The machine has more than one account, and the folder is shared by
    /// all of them: the last one to uninstall is the one that may remove
    /// it. `TASK_ENUM_HIDDEN` is not optional here — our own task is
    /// hidden, so an enumeration without it reports an empty folder that
    /// is not empty. `DeleteFolder` refuses a non-empty folder anyway,
    /// which is the backstop, but a delete that is expected to fail is
    /// not a check.
    fn remove_folder_if_empty(&self) {
        if !self.scoped {
            return;
        }
        // SAFETY: `self.folder` is live for as long as `self` is, and
        // `tasks` is the collection the first call just returned.
        let empty = unsafe { self.folder.GetTasks(TASK_ENUM_HIDDEN.0) }
            .and_then(|tasks| unsafe { tasks.Count() })
            .is_ok_and(|count| count == 0);
        if empty {
            // Best effort: a folder we cannot remove is untidy, not
            // broken, and uninstall must not fail over it.
            // SAFETY: `self.root` is live; the flags parameter is reserved
            // and documented as zero.
            let _ = unsafe { self.root.DeleteFolder(&BSTR::from(FOLDER), 0) };
        }
    }
}

/// Registers (or updates) the logon task for the calling user.
///
/// `programs` are launched in order at every logon of this account. The
/// engine goes first and the renderer second: the renderer is the
/// watchdog (DESIGN 4.3), and a watchdog that starts before the thing it
/// watches spends its first moments restarting a process that was about
/// to start anyway.
///
/// This must run **as the interactive user**, not under an installer's
/// elevated token — an elevated process belongs to a different account,
/// and a task registered from it would fire at the wrong user's logon
/// while the install reported success (DESIGN 12.2). The caller is
/// responsible for that; there is no way to detect it reliably from here.
///
/// Requires an initialized apartment ([`crate::ComApartment`]).
pub fn register(programs: &[&Path]) -> Result<()> {
    if programs.is_empty() {
        // A task with no actions registers successfully and does nothing,
        // which would leave the account looking configured and without an
        // IME.
        return Err(Error::from_hresult(E_INVALIDARG));
    }

    let service = connect()?;
    let account = current_account()?;
    let location = Location::resolve(&service, &account, true)?;

    // SAFETY: `service` is connected; the flags parameter is reserved and
    // documented as zero.
    let task = unsafe { service.NewTask(0) }?;

    // SAFETY: every call below is on `task` or on an interface `task` just
    // handed back, and each `BSTR`/`VARIANT` is a temporary that lives
    // until the call it is passed to returns.
    unsafe {
        let info = task.RegistrationInfo()?;
        info.SetAuthor(&BSTR::from("Sakura Input"))?;
        info.SetDescription(&BSTR::from(
            "Starts the Sakura Input engine and renderer at logon. \
             Removing this task disables the IME for this account.",
        ))?;

        let principal = task.Principal()?;
        principal.SetUserId(&BSTR::from(account.as_str()))?;
        principal.SetLogonType(TASK_LOGON_INTERACTIVE_TOKEN)?;
        principal.SetRunLevel(TASK_RUNLEVEL_LUA)?;

        let settings = task.Settings()?;
        // "PT0S" is the Task Scheduler's spelling of "no limit". Without
        // it the engine is killed after three days of uptime.
        settings.SetExecutionTimeLimit(&BSTR::from("PT0S"))?;
        settings.SetDisallowStartIfOnBatteries(VARIANT_FALSE)?;
        settings.SetStopIfGoingOnBatteries(VARIANT_FALSE)?;
        settings.SetStartWhenAvailable(VARIANT_TRUE)?;
        settings.SetMultipleInstances(TASK_INSTANCES_IGNORE_NEW)?;
        settings.SetHidden(VARIANT_TRUE)?;
        settings.IdleSettings()?.SetStopOnIdleEnd(VARIANT_FALSE)?;
        // A backstop only. One minute is far too slow to be a user-facing
        // recovery story, which is why the renderer watchdog exists; this
        // catches the case where the renderer died too.
        settings.SetRestartCount(3)?;
        settings.SetRestartInterval(&BSTR::from("PT1M"))?;

        let trigger = task.Triggers()?.Create(TASK_TRIGGER_LOGON)?;
        let logon: ILogonTrigger = trigger.cast()?;
        logon.SetId(&BSTR::from("SakuraInputLogon"))?;
        // Scoped to this account: without a user id the trigger fires for
        // every user who logs on, and each of them would run this user's
        // copy of the engine.
        logon.SetUserId(&BSTR::from(account.as_str()))?;
        // No start delay. The IME must be there for the first keystroke
        // after logon; a delay would mean text typed in the first seconds
        // goes through raw, which is exactly the case where a user is
        // typing a password-adjacent field into a freshly restored app.

        let actions = task.Actions()?;
        for program in programs {
            let action: IExecAction = actions.Create(TASK_ACTION_EXEC)?.cast()?;
            // Paths go through UTF-16 rather than `String`, because a path
            // is not required to be valid Unicode and lossy conversion
            // would produce a task that launches a file name that does not
            // exist.
            action.SetPath(&bstr(program.as_os_str()))?;
            if let Some(dir) = program.parent() {
                action.SetWorkingDirectory(&bstr(dir.as_os_str()))?;
            }
        }

        location.folder.RegisterTaskDefinition(
            &BSTR::from(location.name.as_str()),
            &task,
            TASK_CREATE_OR_UPDATE.0,
            &VARIANT::default(),
            &VARIANT::default(),
            TASK_LOGON_INTERACTIVE_TOKEN,
            &VARIANT::default(),
        )?;
    }

    Ok(())
}

/// Removes the logon task for the calling user.
///
/// A task that was never registered is not an error: uninstall runs this
/// unconditionally, and a machine where registration failed halfway must
/// still uninstall cleanly.
pub fn unregister() -> Result<()> {
    let service = connect()?;
    let account = current_account()?;
    let location = Location::resolve(&service, &account, false)?;

    // SAFETY: `location.folder` is live; the flags parameter is reserved
    // and documented as zero.
    let removed = match unsafe {
        location
            .folder
            .DeleteTask(&BSTR::from(location.name.as_str()), 0)
    } {
        Ok(()) => Ok(()),
        Err(error) if error.code() == NOT_FOUND => Ok(()),
        Err(error) => Err(error),
    };
    location.remove_folder_if_empty();
    removed
}

/// Whether the logon task is registered for the calling user.
///
/// The logon stub uses this to self-repair after a Windows feature update
/// (DESIGN 12.2), so "cannot tell" must read as "not registered" — a
/// redundant re-registration is harmless, a missed one leaves the account
/// without an IME.
pub fn is_registered() -> bool {
    let Ok(service) = connect() else {
        return false;
    };
    let Ok(account) = current_account() else {
        return false;
    };
    let Ok(location) = Location::resolve(&service, &account, false) else {
        return false;
    };
    // SAFETY: `location.folder` is live for the rest of this function.
    unsafe {
        location
            .folder
            .GetTask(&BSTR::from(location.name.as_str()))
            .is_ok()
    }
}

fn connect() -> Result<ITaskService> {
    // SAFETY: `CoCreateInstance` needs an initialized apartment, which the
    // caller holds (see the module's Safety note); `service` is then a live
    // interface, and the four VARIANTs outlive the `Connect` call.
    unsafe {
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
        // Four empty VARIANTs mean "this machine, as me". Anything else
        // would be a remote or impersonated connection, which is not what
        // a per-user launcher wants.
        service.Connect(
            &VARIANT::default(),
            &VARIANT::default(),
            &VARIANT::default(),
            &VARIANT::default(),
        )?;
        Ok(service)
    }
}

/// The calling user as `DOMAIN\user`, which is the form both the
/// principal and the logon trigger expect.
///
/// Public because the account this resolves to is the one every per-user
/// action lands on, and a caller about to take such an action needs to be
/// able to check *which* account that is before taking it — under an
/// elevated or service token it is not necessarily the person at the
/// keyboard (DESIGN 12.1).
pub fn current_account() -> Result<String> {
    let mut len = 0u32;
    // The first call is expected to fail; it exists to size the buffer.
    // SAFETY: a null buffer with a valid length out-parameter is the
    // documented way to ask for the required size.
    let sized = unsafe { GetUserNameExW(NameSamCompatible, None, &mut len) };
    if !sized {
        let error = Error::from_thread();
        if error.code() != HRESULT::from_win32(ERROR_MORE_DATA.0) {
            return Err(error);
        }
    }

    let mut buf = vec![0u16; len as usize];
    // SAFETY: `buf` holds exactly the `len` UTF-16 units the sizing call
    // asked for, and both it and `len` outlive the call.
    if !unsafe { GetUserNameExW(NameSamCompatible, Some(PWSTR(buf.as_mut_ptr())), &mut len) } {
        return Err(Error::from_thread());
    }
    buf.truncate(len as usize);
    Ok(String::from_utf16_lossy(&buf))
}

/// A `BSTR` from an OS string, losing nothing on the way.
fn bstr(s: &std::ffi::OsStr) -> BSTR {
    // `BSTR` is length-prefixed, so the wide form must not carry a
    // terminator; `os_to_wide` is the counted one of the pair.
    BSTR::from_wide(&crate::wide::os_to_wide(s))
}

/// Makes an account name usable as a task name.
///
/// Task names may not contain the path separators the Task Scheduler uses
/// to address them, and `DOMAIN\user` contains one. Everything else is
/// left alone so the name still says whose task it is.
fn sanitize(account: &str) -> String {
    account
        .chars()
        .map(|c| {
            if matches!(c, '\\' | '/' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_domain_qualified_account_becomes_a_usable_task_name() {
        assert_eq!(sanitize(r"CONTOSO\alice"), "CONTOSO-alice");
        assert_eq!(sanitize("alice"), "alice");
    }

    /// A task name that still contained a separator would address a
    /// *folder*, and registration would either fail or file the task
    /// somewhere nobody looks for it.
    #[test]
    fn a_sanitized_name_can_never_address_a_folder() {
        for account in [r"A\B\C", "A/B", "A:B", r"\\host\user"] {
            let name = sanitize(account);
            assert!(!name.contains(['\\', '/', ':']), "{name}");
        }
    }

    #[test]
    fn the_account_lookup_returns_a_domain_qualified_name() {
        let account = current_account().expect("this process has a user");
        assert!(!account.is_empty());
        assert!(!account.contains('\0'), "the trailing nul must be trimmed");
    }
}
