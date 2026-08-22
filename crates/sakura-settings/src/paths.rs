//! Canonical per-user settings paths.

use std::io;
use std::path::PathBuf;

fn local_root() -> io::Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "LOCALAPPDATA is unavailable for Sakura Input settings",
        )
    })?;
    Ok(PathBuf::from(local).join("SakuraInput"))
}

pub fn configuration() -> io::Result<PathBuf> {
    Ok(local_root()?.join("config").join("config.toml"))
}

pub fn user_dictionary() -> io::Result<PathBuf> {
    Ok(local_root()?.join("userdict").join("user.tsv"))
}

pub fn learning() -> io::Result<PathBuf> {
    Ok(local_root()?.join("learning").join("log.bin"))
}

pub fn input_history() -> io::Result<PathBuf> {
    Ok(local_root()?.join("history").join("input.bin"))
}

pub fn timeout_diagnostics() -> io::Result<PathBuf> {
    Ok(local_root()?.join("diagnostics").join("ipc-timeouts.bin"))
}

pub fn debug_trace() -> io::Result<PathBuf> {
    Ok(local_root()?.join("logs").join("debug.tsv"))
}

pub fn update_preferences() -> io::Result<PathBuf> {
    Ok(local_root()?.join("update").join("settings.txt"))
}

pub fn update_installer() -> io::Result<PathBuf> {
    Ok(local_root()?
        .join("update")
        .join("sakura_setup.pending.exe"))
}

pub fn update_log() -> io::Result<PathBuf> {
    Ok(local_root()?.join("update").join("install.log"))
}
