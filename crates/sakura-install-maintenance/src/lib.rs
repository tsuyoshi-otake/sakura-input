//! Installation maintenance for Sakura Input: the per-user logon launcher
//! task, the elevated payload-cleanup task and the cleanup it runs, and the
//! Windows Error Reporting dump policies (Sakura processes and the explicit
//! VS Code host opt-in).
//!
//! Everything here is driven by `sakura_regtool.exe` or `sakura_logon.exe`
//! during install, uninstall, upgrade, or an administrator's explicit request.
//! None of it is linked into the TSF DLL or runs on the input path. Registry
//! primitives come from `sakura-reg`, which keeps ownership of the layout.

#![cfg(windows)]

pub mod diagnostics;
pub mod launcher;
pub mod maintenance;
pub mod payloads;
pub mod vscode_diagnostics;
