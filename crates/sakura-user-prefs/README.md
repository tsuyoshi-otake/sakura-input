# sakura-user-prefs

Per-user AI text preferences and the Credential Manager API-key store, split from `sakura-reg` in Phase 3.8 (#206).

## Purpose

Keep the one secret store and the small cross-bitness user preferences in a crate a security review can read on its own, without the TSF/COM registration code.

## Owns

`AiTextKey` (the HKCU DWORD read by the TSF key path), `AiTextPreferences` and its provider/auth/style/effort/service-tier values, their HKCU read/write functions, and the Credential Manager API-key functions (`api_key_is_saved`, `read_api_key`, `write_api_key`, `clear_api_key`).

## Must not own

GUIDs, COM/TSF registration, registry layout primitives (`RegKey`, `RegistryView` stay in `sakura-reg`), Task Scheduler tasks, WER policy, configuration-file parsing, or AI request/transport logic.

## Allowed dependencies

`sakura-reg` (registry primitives and wide-string helpers), `windows-core`, and `windows` with `Win32_Foundation`, `Win32_Security_Credentials`, and `Win32_System_Registry`.

## Allowed consumers

`sakura-tsf` (AI key read on the key path), `sakura-engine` (AI text preferences and API key), and `sakura-settings` (preference editing).

## Public API budget

At most 8 public types and 8 public functions. Every public function reads, writes, or clears one persisted user value.

## Issue types

AI text key and preference persistence, API-key storage, and HKCU value compatibility across 32-bit and 64-bit hosts.

## Test commands

Run `cargo test -p sakura-user-prefs --locked` through `ci/run-test-quiet.ps1`, then `ci/dep-policy.ps1`.
