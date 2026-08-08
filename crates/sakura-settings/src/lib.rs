//! Durable settings operations shared by the native control panel and CLI.
//!
//! Keeping file formats and transactional writes below both frontends makes
//! every settings action scriptable and testable without clicking a window,
//! while the shipped executable remains a normal Win32 control panel when it
//! is launched without arguments.

#![cfg(windows)]

/// Environment variable carrying the directory the user ran the command from.
///
/// The install-root bootstrap starts the versioned payload with its working
/// directory set to the payload's own folder under Program Files, so the two
/// binaries need this to agree on where a relative file operand points.
pub const CALLER_DIRECTORY_VARIABLE: &str = "SAKURA_SETTINGS_CALLER_DIRECTORY";

pub mod configuration;
pub mod diagnostics;
pub mod formats;
pub mod input_history;
pub mod learning;
pub mod paths;
pub mod storage;
pub mod updater;
pub mod user_dictionary;
