# Mutation analysis — developer-history hot-enable

## Scope

Configured in `verification/developer-history/cargo-mutants.toml` and intended
to cover:

- `crates/sakura-engine/src/server.rs` — `runtime_services` history branch
- `crates/sakura-engine/src/dispatch.rs` — `set_input_history(Option)`, record gates
- `crates/sakura-engine/src/input_history.rs` — admission / queue / clear epoch
- `crates/sakura-settings/src/cli.rs` — terminal + settle retry

## Score

| Host | Result |
|---|---|
| This Linux cloud agent | **Not executed.** `sakura-engine` is `#![cfg(windows)]`; `cargo-mutants` cannot compile the library here. |
| Windows follow-up | Run the commands in `cargo-mutants.toml` comments; record kill/survive counts here. |

Campaign score on this run: **N/A (environment-blocked)**. Residual risk is
accepted with the independent oracle, TLC Safety, and new hot-enable unit /
pipe tests as primary killers for the attach no-op mutant class.

## Surviving mutants

None measured on this host. Expected survivors after a Windows run should be
classified below rather than left unlabeled.

## Equivalent-mutant classification (predicted)

| Mutant class | Likely status | Why |
|---|---|---|
| Attach becomes no-op (`set_input_history` ignores `Some`) | **Must die** | `optional_input_history_follows…`, `hot_attach_and_detach…`, pipe hot-enable |
| `developer_history_terminal` still returns `restart-required-to-enable` | **Must die** | CLI unit test asserts absence |
| Drop `request_after_publish` clear on Shutdown in oracle | Equivalent / low value | Oracle-only; TLC uses live-gated invariant |
| Cosmetic `Debug` / log string edits | Equivalent | No behavioral oracle |

## Killer tests (already present)

- `optional_input_history_follows_a_live_developer_mode_change`
- `hot_attach_and_detach_of_developer_history_matches_stats_active`
- `live_engine_hot_enables_developer_history_and_records_a_normal_key` (Windows)
- `developer_history_terminal_state_never_claims_a_saved_setting_is_already_active`
- Oracle / PBT / TLC ForbiddenStaleInactive
