//! Stable settings entry point for the side-by-side installation layout.
//!
//! The real settings process lives beside the active engine and TSF DLL.  This
//! tiny process is intentionally kept at the install root so shortcuts and
//! documentation never need to change when a new version is installed.  It
//! exits after handing GUI launches and update commands to the versioned
//! payload, so the installer never tries to overwrite this root bootstrap
//! while it is performing a side-by-side update. Synchronous administrative
//! CLI commands still wait for their child and preserve their exit status.

#![cfg(windows)]

use std::ffi::OsString;
use std::os::windows::process::CommandExt;
use std::process::Command;

use sakura_reg::{com_server, RegistryView};
use sakura_settings::CALLER_DIRECTORY_VARIABLE;

const PAYLOAD_NAME: &str = "sakura_settings_payload.exe";
// The payload binary also serves the scriptable CLI and therefore remains a
// console-subsystem executable.  GUI launches must explicitly suppress a new
// console; CREATE_NO_WINDOW is inherited neither from the TSF caller nor from
// this short-lived bootstrap process.
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn main() {
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    let payload = match active_payload() {
        Ok(path) => path,
        Err(message) => {
            eprintln!("sakura_settings: {message}");
            std::process::exit(1);
        }
    };

    let mut command = Command::new(&payload);
    command.args(&arguments).current_dir(
        payload
            .parent()
            .expect("an active payload executable has a parent directory"),
    );
    // The payload runs from its own versioned directory under Program Files,
    // which is not where the user typed the command and is not writable by
    // them. Hand over the real working directory so file operands such as
    // `learning export report.tsv` still land where the user expects.
    if let Ok(directory) = std::env::current_dir() {
        command.env(CALLER_DIRECTORY_VARIABLE, directory);
    }
    if is_gui_or_update_launch(&arguments) {
        command.creation_flags(CREATE_NO_WINDOW);
        if let Err(error) = command.spawn() {
            eprintln!(
                "sakura_settings: could not start {}: {error}",
                payload.display()
            );
            std::process::exit(1);
        }
        return;
    }
    let status = match command.status() {
        Ok(status) => status,
        Err(error) => {
            eprintln!(
                "sakura_settings: could not start {}: {error}",
                payload.display()
            );
            std::process::exit(1);
        }
    };

    std::process::exit(status.code().unwrap_or(1));
}

fn is_update_apply(arguments: &[OsString]) -> bool {
    arguments.len() == 2 && arguments[0] == "update" && arguments[1] == "apply"
}

fn is_gui_or_update_launch(arguments: &[OsString]) -> bool {
    arguments.is_empty() || is_update_apply(arguments)
}

fn active_payload() -> Result<std::path::PathBuf, String> {
    let directory = com_server::registered_payload_dir(RegistryView::Native)
        .map_err(|error| format!("could not read the active TSF registration: {error}"))?
        .ok_or_else(|| "Sakura Input is not registered".to_owned())?;
    let path = directory.join(PAYLOAD_NAME);
    if !path.is_file() {
        return Err(format!(
            "active settings payload is missing: {}",
            path.display()
        ));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::is_gui_or_update_launch;
    use std::ffi::OsString;

    #[test]
    fn gui_and_update_payloads_are_console_free_but_cli_keeps_console_io() {
        assert!(is_gui_or_update_launch(&[]));
        assert!(is_gui_or_update_launch(&[
            OsString::from("update"),
            OsString::from("apply"),
        ]));
        assert!(!is_gui_or_update_launch(&[
            OsString::from("config"),
            OsString::from("show"),
        ]));
    }
}
