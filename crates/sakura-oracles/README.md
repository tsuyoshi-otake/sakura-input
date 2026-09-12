# `sakura-oracles`

## Purpose

Keep independent Rust reference oracles for engine TLA+ correspondence and regression campaigns outside production builds.

## Owns

The current three oracle modules, their three self-test modules, deterministic campaign helpers, atomic-condition probes, and oracle-only fixture writers.

## Must not own

Production behavior, runtime integration, protocol transport, persistence services, Windows APIs, or any item referenced outside development and test configurations.

## Allowed dependencies

The current crate uses only `std`. The architecture plan permits development-only use of `sakura-values` and `proptest` when a future oracle campaign requires them. It does not need the Phase 2.3 reranker protocol crate. A dependency on `sakura-engine` is deliberately absent because engine consumes this crate as a dev-dependency and a reverse edge would create a cycle.

## Allowed consumers

`sakura-engine` as a dev-dependency, oracle artifact generators, verification scripts, and explicitly scoped cargo-mutants campaigns.

## Public API budget

At most 12 public modules. Public functions are limited to oracle transitions, predicates, complete-sequence evaluation, and atomic-condition observation. This extraction exposes three modules and adds no functions or fields to the moved source.

## Non-release ownership

This crate is development-only. `sakura-engine` must not list it under normal dependencies, and release dependency files must contain no oracle source or crate entry. Moving an oracle here does not make its algorithm production policy.

## Issue types

History, Shift+Latin, and space-key ordering changes start in the corresponding `*_oracle.rs` transition and predicate modules. Their adjacent self-tests establish the reference behavior; engine `tests/unit/*_order_tests.rs` and `space_key_dispatch_tests.rs` compare the runtime against it. The corresponding `verification/developer-history`, `verification/shift-latin-order`, and `verification/space-key-dispatch` directories own formal and campaign evidence. These development-only sources are not part of the production read sets in `verification/irv/benchmarks.json`.

## Test commands

Run from the repository root using PowerShell 7:

```powershell
$env:CARGO_HTTP_CHECK_REVOKE = 'false'
./ci/run-test-quiet.ps1 -Name 'engine and oracle tests' -Command { cargo test -p sakura-engine -p sakura-oracles --locked }
./ci/check-process-clean.ps1 -RepositoryRoot (Get-Location)
./ci/check-dependency-rules.ps1 -SelfTest
./ci/check-dependency-rules.ps1 -Advisory -Enforce R9
cargo build -p sakura-engine --release --locked
cargo tree -p sakura-engine -e normal --locked
./ci/check-process-clean.ps1 -RepositoryRoot (Get-Location)
```

Inspect the fresh engine release `.d` file under the configured target triple: it and the normal dependency tree must exclude oracle sources. This proves dependency exclusion, not a binary symbol scan. Run the bounded space-key mutation campaign with `--manifest-path crates/sakura-oracles/Cargo.toml`, its configuration in `verification/space-key-dispatch/cargo-mutants.toml`, `--timeout 60 --build-timeout 60 --jobs 1`, and the quiet wrapper plus process cleanup. Its score measures the independent oracle only, not production dispatch.
