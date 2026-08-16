# Mutation analysis — developer-history hot-enable

## Scope

Windows host run on 2026-08-16 against branch
`cursor/developer-history-hot-enable-5eeb`, cargo-mutants **27.1.0**.

Focused campaign on `Dispatcher::set_input_history`:

```text
cargo mutants -p sakura-engine
  --file crates/sakura-engine/src/dispatch.rs
  --re "Dispatcher::set_input_history"
  --test-tool=cargo --timeout 180 --jobs 2 -- --lib
```

Log: `verification/developer-history/mutants-set-input-history.log`.

## Score

### Hot-enable-critical (`set_input_history` body)

| Mutant | Result |
|---|---|
| replace `Dispatcher::set_input_history` with `()` | **Caught** |
| replace `!=` with `==` (changed check) | **Caught** |
| delete `!` (early-return polarity) | **Caught** |
| replace `&&` with `\|\|` (attaching predicate) | **Caught** |

**Kill rate for the four attach/detach mutants: 4/4 (100%).**

### Broader run (includes Preferences field-delete noise)

Earlier wider `--re` also matched `create_session` / `probe_key` Preferences
struct literals and prediction/reranker arms. Those inflate misses and are
out of campaign scope.

| Metric (wide run) | Value |
|---|---:|
| Total | 44 |
| Caught | 12 |
| Missed | 31 |
| Unviable | 1 |
| Kill rate | 27.9% |

## Surviving mutants

| Mutant class | Classification |
|---|---|
| Preferences field deletes (`developer_mode` included) in create_session / probe_key | **Equivalent / low value** — attach path uses `set_input_history`, not that preference mirror |
| prediction/reranker boolean flips inside `runtime_services` | **Out of scope** |

## Killer tests

- `optional_input_history_follows_a_live_developer_mode_change`
- `hot_attach_and_detach_of_developer_history_matches_stats_active` (includes session-id stability across owner swaps)
- `live_engine_hot_enables_developer_history_and_records_a_normal_key`
- `developer_history_terminal_state_never_claims_a_saved_setting_is_already_active`
- Oracle / PBT / TLC `ForbiddenStaleInactive`
