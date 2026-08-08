//! The elevated machine-wide task that retries payload cleanup at logon.
//!
//! The resident IME task intentionally runs at the interactive user's level
//! so it can communicate with normal-integrity applications. That task cannot
//! remove a locked file under `Program Files`, and elevating it would break
//! UIPI compatibility. Cleanup therefore has its own hidden Task Scheduler
//! entry, installed by the administrator and run as `SYSTEM` for every logon.

use std::path::Path;

use windows::Win32::Foundation::{E_UNEXPECTED, VARIANT_BOOL};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::System::TaskScheduler::{
    IExecAction, ILogonTrigger, IRegisteredTask, ITaskService, TaskScheduler, TASK_ACTION_EXEC,
    TASK_CREATE_OR_UPDATE, TASK_INSTANCES_IGNORE_NEW, TASK_LOGON_SERVICE_ACCOUNT,
    TASK_RUNLEVEL_HIGHEST, TASK_TRIGGER_LOGON,
};
use windows::Win32::System::Variant::VARIANT;
use windows_core::{Error, Interface, Result, BSTR, HRESULT};

const FOLDER: &str = "Sakura Input Maintenance";
const TASK_LEAF: &str = "Payload Cleanup";
const TASK_PATH: &str = "\\Sakura Input Maintenance\\Payload Cleanup";
const SYSTEM_ACCOUNT: &str = "SYSTEM";
const ACTION_ARGUMENTS: &str = "--cleanup-payloads";
const EXECUTION_TIME_LIMIT: &str = "PT5M";
const TRIGGER_ID: &str = "SakuraInputPayloadCleanup";
const ALREADY_EXISTS: HRESULT = HRESULT(0x8007_00B7u32 as i32);
const NOT_FOUND: HRESULT = HRESULT(0x8007_0002u32 as i32);

/// Installs or updates the elevated cleanup task.
///
/// The caller must hold an initialized COM apartment and administrator
/// privileges. The task action is a stable root executable, so updates never
/// leave a scheduled task pointing into an obsolete version directory.
pub fn register_cleanup_task(executable: &Path) -> Result<()> {
    let working_directory = executable.parent().ok_or_else(|| {
        Error::new(
            E_UNEXPECTED,
            "the cleanup executable does not have a parent directory",
        )
    })?;
    if !executable.is_absolute() || !executable.is_file() {
        return Err(Error::new(
            E_UNEXPECTED,
            "the cleanup executable must be an existing absolute file",
        ));
    }

    let service = connect()?;
    // SAFETY: `service` is a live Task Scheduler connection and the root
    // folder is present on every local Windows installation.
    let root = unsafe { service.GetFolder(&BSTR::from("\\")) }?;
    // SAFETY: `root` is live; an empty VARIANT requests inherited security.
    let folder = match unsafe { root.CreateFolder(&BSTR::from(FOLDER), &VARIANT::default()) } {
        Ok(folder) => folder,
        Err(error) if error.code() == ALREADY_EXISTS => {
            // SAFETY: the preceding error established that the folder exists.
            unsafe { root.GetFolder(&BSTR::from(FOLDER)) }?
        }
        Err(error) => return Err(error),
    };
    // SAFETY: `service` is connected and the flags argument is reserved zero.
    let task = unsafe { service.NewTask(0) }?;

    // SAFETY: `task` and every interface obtained from it are live. Each
    // temporary BSTR/VARIANT outlives the COM call receiving it.
    unsafe {
        let info = task.RegistrationInfo()?;
        info.SetAuthor(&BSTR::from("Sakura Input"))?;
        info.SetDescription(&BSTR::from(
            "Retries cleanup of inactive Sakura Input payload generations at logon.",
        ))?;

        let principal = task.Principal()?;
        principal.SetUserId(&BSTR::from(SYSTEM_ACCOUNT))?;
        principal.SetLogonType(TASK_LOGON_SERVICE_ACCOUNT)?;
        principal.SetRunLevel(TASK_RUNLEVEL_HIGHEST)?;

        let settings = task.Settings()?;
        settings.SetExecutionTimeLimit(&BSTR::from(EXECUTION_TIME_LIMIT))?;
        settings.SetStartWhenAvailable(true.into())?;
        settings.SetMultipleInstances(TASK_INSTANCES_IGNORE_NEW)?;
        settings.SetHidden(true.into())?;

        let trigger = task.Triggers()?.Create(TASK_TRIGGER_LOGON)?;
        let logon: ILogonTrigger = trigger.cast()?;
        logon.SetId(&BSTR::from(TRIGGER_ID))?;

        let actions = task.Actions()?;
        let action: IExecAction = actions.Create(TASK_ACTION_EXEC)?.cast()?;
        action.SetPath(&bstr(executable.as_os_str()))?;
        action.SetArguments(&BSTR::from(ACTION_ARGUMENTS))?;
        action.SetWorkingDirectory(&bstr(working_directory.as_os_str()))?;

        let registered = folder.RegisterTaskDefinition(
            &BSTR::from(TASK_LEAF),
            &task,
            TASK_CREATE_OR_UPDATE.0,
            &VARIANT::default(),
            &VARIANT::default(),
            TASK_LOGON_SERVICE_ACCOUNT,
            &VARIANT::default(),
        )?;
        drop(registered);

        // Do not trust only the HRESULT returned by registration. Read the
        // persisted task back through Task Scheduler and verify every field
        // that controls when and with which authority our executable runs.
        // If the service canonicalized an unexpected definition (or a policy
        // product rewrote it), installation must fail instead of logging a
        // success for a task that will never safely clean old payloads.
        let verification = folder
            .GetTask(&BSTR::from(TASK_LEAF))
            .and_then(|registered| verify_cleanup_task(&registered, executable));
        if let Err(error) = verification {
            // A definition that failed verification must not remain as an
            // untrusted SYSTEM execution path. Deletion is best effort; the
            // original verification error is the actionable failure.
            let _ = folder.DeleteTask(&BSTR::from(TASK_LEAF), 0);
            return Err(error);
        }
    }

    Ok(())
}

/// Verifies the definition after Task Scheduler has persisted and normalized it.
///
/// This is deliberately stricter than checking that a task with the right name
/// exists. The action path and arguments are a SYSTEM trust boundary, while the
/// trigger and settings make cleanup bounded and repeatable.
fn verify_cleanup_task(registered: &IRegisteredTask, executable: &Path) -> Result<()> {
    // SAFETY: `registered` is the live object read back from Task Scheduler.
    // Every output value below is initialized before its address is passed and
    // remains alive for the complete COM call. All child interfaces are owned
    // references obtained from that same registered definition.
    unsafe {
        ensure(
            registered.Name()? == TASK_LEAF,
            "registered cleanup task has an unexpected name",
        )?;
        ensure(
            same_text(&registered.Path()?.to_string(), TASK_PATH),
            "registered cleanup task has an unexpected path",
        )?;
        ensure(
            registered.Enabled()?.as_bool(),
            "registered cleanup task is disabled",
        )?;

        let definition = registered.Definition()?;
        let principal = definition.Principal()?;
        let mut user_id = BSTR::new();
        principal.UserId(&mut user_id)?;
        ensure(
            is_system_account(&user_id.to_string()),
            "registered cleanup task does not run as SYSTEM",
        )?;
        let mut logon_type = TASK_LOGON_SERVICE_ACCOUNT;
        principal.LogonType(&mut logon_type)?;
        ensure(
            logon_type == TASK_LOGON_SERVICE_ACCOUNT,
            "registered cleanup task has an unexpected logon type",
        )?;
        let mut run_level = TASK_RUNLEVEL_HIGHEST;
        principal.RunLevel(&mut run_level)?;
        ensure(
            run_level == TASK_RUNLEVEL_HIGHEST,
            "registered cleanup task does not use the highest run level",
        )?;

        let actions = definition.Actions()?;
        let mut action_count = 0;
        actions.Count(&mut action_count)?;
        ensure(
            action_count == 1,
            "registered cleanup task must contain exactly one action",
        )?;
        let action = actions.get_Item(1)?;
        let mut action_type = TASK_ACTION_EXEC;
        action.Type(&mut action_type)?;
        ensure(
            action_type == TASK_ACTION_EXEC,
            "registered cleanup task action is not an executable",
        )?;
        let action: IExecAction = action.cast()?;
        let mut action_path = BSTR::new();
        let mut action_arguments = BSTR::new();
        let mut action_working_directory = BSTR::new();
        action.Path(&mut action_path)?;
        action.Arguments(&mut action_arguments)?;
        action.WorkingDirectory(&mut action_working_directory)?;
        ensure(
            same_text(&action_path.to_string(), &executable.to_string_lossy()),
            "registered cleanup task points at an unexpected executable",
        )?;
        ensure(
            action_arguments == ACTION_ARGUMENTS,
            "registered cleanup task has unexpected arguments",
        )?;
        let expected_working_directory = executable.parent().ok_or_else(|| {
            Error::new(
                E_UNEXPECTED,
                "the cleanup executable does not have a parent directory",
            )
        })?;
        ensure(
            same_text(
                &action_working_directory.to_string(),
                &expected_working_directory.to_string_lossy(),
            ),
            "registered cleanup task has an unexpected working directory",
        )?;

        let triggers = definition.Triggers()?;
        let mut trigger_count = 0;
        triggers.Count(&mut trigger_count)?;
        ensure(
            trigger_count == 1,
            "registered cleanup task must contain exactly one trigger",
        )?;
        let trigger = triggers.get_Item(1)?;
        let mut trigger_type = TASK_TRIGGER_LOGON;
        trigger.Type(&mut trigger_type)?;
        ensure(
            trigger_type == TASK_TRIGGER_LOGON,
            "registered cleanup task trigger is not a logon trigger",
        )?;
        let mut trigger_id = BSTR::new();
        trigger.Id(&mut trigger_id)?;
        ensure(
            trigger_id == TRIGGER_ID,
            "registered cleanup task has an unexpected trigger id",
        )?;
        let logon: ILogonTrigger = trigger.cast()?;
        let mut trigger_user = BSTR::new();
        logon.UserId(&mut trigger_user)?;
        ensure(
            trigger_user.is_empty(),
            "registered cleanup task is not configured for every user logon",
        )?;

        let settings = definition.Settings()?;
        let mut execution_time_limit = BSTR::new();
        let mut hidden = VARIANT_BOOL::default();
        let mut start_when_available = VARIANT_BOOL::default();
        let mut multiple_instances = TASK_INSTANCES_IGNORE_NEW;
        settings.ExecutionTimeLimit(&mut execution_time_limit)?;
        settings.Hidden(&mut hidden)?;
        settings.StartWhenAvailable(&mut start_when_available)?;
        settings.MultipleInstances(&mut multiple_instances)?;
        ensure(
            execution_time_limit == EXECUTION_TIME_LIMIT,
            "registered cleanup task has an unexpected execution time limit",
        )?;
        ensure(hidden.as_bool(), "registered cleanup task is not hidden")?;
        ensure(
            start_when_available.as_bool(),
            "registered cleanup task will not start when a logon run was missed",
        )?;
        ensure(
            multiple_instances == TASK_INSTANCES_IGNORE_NEW,
            "registered cleanup task has an unsafe multiple-instance policy",
        )?;

        Ok(())
    }
}

fn ensure(condition: bool, message: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Error::new(E_UNEXPECTED, message))
    }
}

fn same_text(left: &str, right: &str) -> bool {
    left.eq_ignore_ascii_case(right)
}

fn is_system_account(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_uppercase().as_str(),
        "SYSTEM" | "NT AUTHORITY\\SYSTEM" | "S-1-5-18"
    )
}

/// Removes the cleanup task. Missing task/folder is already the desired state.
pub fn unregister_cleanup_task() -> Result<()> {
    let service = connect()?;
    // SAFETY: `service` is a live Task Scheduler connection.
    let root = unsafe { service.GetFolder(&BSTR::from("\\")) }?;
    // SAFETY: `root` is live and the folder lookup only reads Task Scheduler.
    let Ok(folder) = (unsafe { root.GetFolder(&BSTR::from(FOLDER)) }) else {
        return Ok(());
    };
    // SAFETY: `folder` is live and the flags argument is reserved zero.
    match unsafe { folder.DeleteTask(&BSTR::from(TASK_LEAF), 0) } {
        Ok(()) => Ok(()),
        Err(error) if error.code() == NOT_FOUND => Ok(()),
        Err(error) => Err(error),
    }
}

fn connect() -> Result<ITaskService> {
    // SAFETY: the caller owns a live COM apartment; all VARIANTs outlive the
    // Connect call and the returned interface remains owned by this function.
    unsafe {
        let service: ITaskService = CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER)?;
        service.Connect(
            &VARIANT::default(),
            &VARIANT::default(),
            &VARIANT::default(),
            &VARIANT::default(),
        )?;
        Ok(service)
    }
}

fn bstr(s: &std::ffi::OsStr) -> BSTR {
    BSTR::from_wide(&crate::wide::os_to_wide(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_account_aliases_are_accepted_but_other_accounts_are_not() {
        assert!(is_system_account("SYSTEM"));
        assert!(is_system_account("nt authority\\system"));
        assert!(is_system_account("S-1-5-18"));
        assert!(!is_system_account("developer"));
        assert!(!is_system_account("LOCAL SERVICE"));
    }

    #[test]
    fn task_paths_compare_case_insensitively_but_not_by_suffix() {
        assert!(same_text(
            "C:\\Program Files\\Sakura Input\\sakura_regtool.exe",
            "c:\\program files\\sakura input\\SAKURA_REGTOOL.EXE"
        ));
        assert!(!same_text(
            "C:\\Temp\\sakura_regtool.exe",
            "C:\\Program Files\\Sakura Input\\sakura_regtool.exe"
        ));
    }
}
