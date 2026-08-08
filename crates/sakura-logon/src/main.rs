//! Windowless per-user logon repair and process bootstrap.

#![cfg(windows)]
#![windows_subsystem = "windows"]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use sakura_logon::{execute, write_status, Component};
use sakura_reg::{com_server, launcher, user_profile, ComApartment, RegistryView};

fn main() {
    let executable = std::env::current_exe().ok();
    // The task points at this stable bootstrap, while the actual engine and
    // renderer live in the versioned directory selected by the machine-wide
    // TSF registration.  That lets every user's existing task pick up a new
    // release without rewriting tasks or deleting a DLL still mapped by a
    // different host process.
    let payload = current_payload_dir().or_else(|| {
        // Keep a useful fallback for a developer checkout and for the legacy
        // fixed-layout installer during the one-time migration.
        executable
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    });
    let com = ComApartment::new().ok();

    let outcome = execute(
        || {
            com.as_ref().is_some()
                && executable
                    .as_deref()
                    .is_some_and(|stub| launcher::register_if_missing(&[stub]).is_ok())
        },
        || com.as_ref().is_some() && user_profile::enable().is_ok(),
        |component| launch(payload.as_deref(), component),
    );

    if let Some(path) = status_path() {
        let _ = write_status(&path, outcome);
    }
    std::process::exit(outcome.exit_code());
}

fn current_payload_dir() -> Option<PathBuf> {
    com_server::registered_payload_dir(RegistryView::Native)
        .ok()
        .flatten()
        .filter(|path| path.join("sakura_engine.exe").is_file())
}

fn launch(payload: Option<&Path>, component: Component) -> bool {
    let Some(payload) = payload else {
        return false;
    };
    let leaf = match component {
        Component::Engine => "sakura_engine.exe",
        Component::Renderer => "sakura_renderer.exe",
    };
    Command::new(payload.join(leaf))
        .current_dir(payload)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .is_ok()
}

fn status_path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|local| {
        PathBuf::from(local)
            .join("SakuraInput")
            .join("logs")
            .join("logon.status")
    })
}
