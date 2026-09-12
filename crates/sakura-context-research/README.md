# `sakura-context-research`

## Purpose
Retain the dormant Issue 34 context experiment outside default engine builds.
Track reading cost with the [IRV inventory](../../verification/irv/benchmarks.json).
The SCV1 contract remains owned by [sakura-neural-proto](../sakura-neural-proto/src/lib.rs).

## Owns
Exactly `context_baseline`, `context_evaluation`, `context_intelligence`, and `prediction_snapshot`, including their existing inline unit tests.

## Must not own
Anything referenced by the default build, runtime IPC, engine sessions, current prediction behavior, new research, or unrelated abandoned code.

## Allowed dependencies
`sakura-values` for the exact shared `FixedStr` and `InputScope` definitions, plus `sakura-neural-proto` for its distinct `CandidateAuthority`, 32-byte `Fingerprint`, and existing candidate bounds. No engine dependency is allowed. Revision `d45a1bc` records this dependency boundary before implementation.

## Allowed consumers
Only `sakura-engine` through feature `context-research`. The research budget suite is crate-owned and accesses internals only during this crate's unit-test build.

## Public API budget
Zero public items. The four owned modules are private and their cross-module items use `pub(crate)` only. The dormant engine modules had zero external production callers, so extraction does not invent a compatibility facade.

## Issue types
Changes to the preserved baseline, replay metrics, bounded context state, or snapshot correlation start in the corresponding one of the four owned modules and retain its adjacent unit tests. Feature wiring changes start in the engine `context-research` feature and this crate manifest. Dependency, consumer, lifecycle, or public API changes require an explicit architecture-charter revision; this dormant crate is not an entry point for new production behavior.

## Test commands
Run from the repository root using PowerShell 7:

```powershell
$env:CARGO_HTTP_CHECK_REVOKE = 'false'
./ci/run-test-quiet.ps1 -Name 'context research tests' -Command { cargo test -p sakura-context-research --locked }
./ci/run-test-quiet.ps1 -Name 'engine context research diagnostic' -Command { cargo test -p sakura-engine --lib --features context-research --locked context_research_session_size_tests }
cargo build -p sakura-engine --locked
cargo build -p sakura-engine --features context-research --locked
cargo clippy -p sakura-context-research --all-targets --locked -- -D warnings
cargo clippy -p sakura-engine --all-targets --features context-research --locked -- -D warnings
./ci/check-process-clean.ps1 -RepositoryRoot (Get-Location)
```

Both quiet-wrapper invocations must print one `PASS:` line. The final cleanup command must report no repository-owned Cargo, rustc, clippy, test, engine, or target-directory process remaining.

## Lifecycle
The crate is retained until a separate PR proves the feature unused for one year. Time passing alone never deletes it.

## Verification
Build engine with no features and with `context-research`; run this crate's unit tests and the feature-gated engine Session-size diagnostic; inspect metadata and source references; enforce R14's README/manifest dependency equality when available; confirm default release dependency files omit this crate and literal public item count is zero.
