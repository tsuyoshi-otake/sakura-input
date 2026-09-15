# sakura-install-maintenance

Install-time and administrator maintenance for Sakura Input, split from `sakura-reg` in Phase 3.8 (#206).

## Purpose

Own the operating-system tasks and policies that keep an installation running and clean, so registration code and the TSF DLL do not carry Task Scheduler, cleanup, or crash-dump policy.

## Owns

`launcher` (the per-user logon task), `maintenance` (the elevated SYSTEM cleanup task), `payloads` (inactive side-by-side payload cleanup), `diagnostics` (WER `LocalDumps` for Sakura executables), and `vscode_diagnostics` (the explicit, ownership-marked VS Code WER opt-in).

## Must not own

GUIDs, COM/TSF registration or language profiles, registry layout primitives (`sakura-reg`), user preferences or secrets (`sakura-user-prefs`), pipe or protocol code, or anything linked into the TSF DLL.

## Allowed dependencies

`sakura-reg` (registry primitives and wide-string helpers), `windows-core`, and `windows` with `Win32_Foundation`, `Win32_System_Registry`, `Win32_System_Com`, `Win32_System_TaskScheduler`, `Win32_System_Variant`, `Win32_System_Ole`, and `Win32_Security_Authentication_Identity`.

## Allowed consumers

`sakura-regtool` and `sakura-logon`.

## Public API budget

At most 12 public types and 20 public functions across the five modules. New entry points must be driven by an installer, logon, or explicit administrator command.

## Issue types

Logon launcher registration, payload cleanup after upgrades, and WER dump policy (including the VS Code crash-investigation opt-in).

## Test commands

Run `cargo test -p sakura-install-maintenance --locked` through `ci/run-test-quiet.ps1` (the launcher round trip ignores its Task Scheduler write test by default), then `ci/dep-policy.ps1`.
