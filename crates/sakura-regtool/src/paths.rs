//! Finding the payload.
//!
//! Every path this tool hands to Windows ends up written down somewhere
//! durable — a CLSID's `InprocServer32`, a scheduled task's action — and
//! is then used months later by a different process. Two properties follow
//! and are enforced here rather than at each call site:
//!
//! * **Absolute.** A relative path recorded in the registry resolves
//!   against whatever working directory the *reading* process happens to
//!   have, which is not this one.
//! * **Present.** A registration pointing at a file that is not there is
//!   the brick scenario from DESIGN 12.2 — Windows tries to load the text
//!   service, fails, and the user has no working IME. Checking now costs a
//!   `stat`; finding out later costs the user their keyboard.
//!
//! `std::path::absolute` is used rather than `canonicalize` deliberately:
//! canonicalize returns a `\\?\`-prefixed path, and that prefix is not
//! understood by every consumer of these strings.

use std::path::{Path, PathBuf};

/// The text service DLL, matching the bitness of whoever loads it.
pub const TSF_DLL: &str = "sakura_tsf.dll";
/// The short-lived task action that repairs per-user state before bootstrap.
pub const LOGON_EXE: &str = "sakura_logon.exe";
/// Where the x86 build of the DLL sits in an installed layout. The file
/// name stays the same across architectures; only the directory changes,
/// so nothing has to encode "32" into a name that on ARM64 would be a lie.
pub const WOW64_DIR: &str = "x86";

/// The directory this executable was started from — the installed payload
/// directory, since the installer puts everything in one place.
pub fn payload_dir() -> Result<PathBuf, String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("cannot locate this executable: {error}"))?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| format!("{} has no parent directory", exe.display()))
}

/// A file the command cannot run without.
pub fn required(
    explicit: Option<PathBuf>,
    default_leaf: &str,
    what: &str,
) -> Result<PathBuf, String> {
    match explicit {
        Some(path) => verify(absolute(path)?, what),
        None => {
            let path = payload_dir()?.join(default_leaf);
            verify(path, what)
        }
    }
}

/// A file the command can proceed without, but only when the caller did
/// not name one. An explicitly named path that is missing is a mistake;
/// a default that is missing is an absence.
pub fn optional(
    explicit: Option<PathBuf>,
    default_leaf: &str,
    what: &str,
) -> Result<Option<PathBuf>, String> {
    match explicit {
        Some(path) => verify(absolute(path)?, what).map(Some),
        None => {
            let path = payload_dir()?.join(default_leaf);
            Ok(path.is_file().then_some(path))
        }
    }
}

fn absolute(path: PathBuf) -> Result<PathBuf, String> {
    std::path::absolute(&path)
        .map_err(|error| format!("cannot resolve {}: {error}", path.display()))
}

fn verify(path: PathBuf, what: &str) -> Result<PathBuf, String> {
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!("no {what} at {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_is_refused_before_it_reaches_the_registry() {
        let error = required(
            Some(PathBuf::from("does-not-exist.dll")),
            TSF_DLL,
            "text service DLL",
        )
        .expect_err("a nonexistent path must not be accepted");
        assert!(
            error.contains("text service DLL"),
            "the message must say what was missing: {error}"
        );
    }

    #[test]
    fn an_accepted_path_is_absolute_even_when_given_relative() {
        // `cargo test` runs with the crate root as the working directory,
        // so this relative path resolves — which is the point: it must not
        // stay relative once accepted. Nothing here mutates the working
        // directory, because these tests share a process.
        let resolved = required(Some(PathBuf::from("Cargo.toml")), TSF_DLL, "payload")
            .expect("the crate's own manifest");
        assert!(
            resolved.is_absolute(),
            "{} would resolve against the reader's working directory",
            resolved.display()
        );
        assert!(
            !resolved.to_string_lossy().starts_with(r"\\?\"),
            "the extended-length prefix is not understood by every consumer"
        );
    }

    #[test]
    fn an_absent_default_is_an_absence_but_an_absent_request_is_an_error() {
        assert_eq!(
            optional(None, "no-such-component.exe", "renderer"),
            Ok(None)
        );
        assert!(optional(
            Some(PathBuf::from("no-such.exe")),
            "renderer.exe",
            "renderer"
        )
        .is_err());
    }
}
