# Conversion diagnostics recovery (#434)

## Ownership and scope

- Developer-history collection remains owned by `HistoryRuntime` and its store writer. The preexisting-lock rejection contract is preserved. Settings now reports record count, newest timestamp, and unverified trailing bytes so inactive zero counters cannot be mistaken for fresh evidence.
- `sakura-ipc::debug_trace` owns bounded trace transport and original event metadata. The engine owns the asynchronous writer lifetime; TSF does not start a trace thread. Full queues drop diagnostics rather than block input. Disabling stops new admission; previously admitted rows may persist later. Shutdown discards the backlog and waits for the current file operation; its duration is not bounded against a stalled OS write. Shutdown counters are a snapshot if detached request producers remain active.
- The AI worker maps only allowlisted invalid-request parameter names to fixed error codes. TSF presents actionable messages. Provider messages, account identifiers, and submitted text never enter those codes. Configured model, effort, tier, and terminal no-retry behavior are preserved.
- TSF declines new character/Space/Henkan work when an idle host explicitly reports read-only. Existing composition, pending writes, undo, and recovery retain their terminal owners. Each status query is a re-entry boundary; unknown status keeps the compatibility path. Dynamic status is not cached.

## Executable acceptance rubric

| Criterion | Verify | Expect |
|---|---|---|
| Slow diagnostics cannot block producers | Wrapped `cargo test --locked -p sakura-ipc --lib debug_trace::` | A gated writer remains blocked while producers finish; overflow/shutdown drops are counted and producer metadata survives. |
| A stalled trace does not delay real key replies | Wrapped `cargo test --locked -p sakura-engine --features dev-fixtures --test history_maintenance delayed_trace_writes -- --nocapture` | The isolated real engine delays each trace write by 200 ms, answers synthetic keys within 50 ms, preserves preedit/commit, later persists a complete row, and exits normally. |
| Slow initialization is observed without restarting it | Wrapped `cargo test --locked -p sakura-settings --bin sakura_settings_payload developer_history_` | A simulated 36-second initialization succeeds; polling backs off, timeouts stay unconfirmed/nonzero, transport errors terminate once, and already-settled/offline states do not wait. |
| Fault injection cannot enter shipping operation | Wrapped `cargo test --locked -p sakura-engine --bin sakura_engine trace_write_delay`; `cargo tree --locked --workspace -e features` | Default startup rejects the delay argument; the default workspace excludes IPC `dev-fixtures`. Fixture builds still require a private test pipe and a bounded, unique delay. |
| AI rejection is actionable and private | Wrapped `cargo test --locked -p sakura-ai-worker` | A fixture rejecting `service_tier` makes one request, returns a fixed code, and never falls back or echoes provider text. |
| Read-only status avoids new write work | Wrapped `cargo test --locked -p sakura-tsf --lib read_only_` | Repeated Probe/Apply are declined, writable/unknown status resumes the existing path, pending work and re-entry stay fenced. |
| Regression assertions detect broken behavior | Temporarily substitute blocking trace send, generic HTTP classification, and removal of the read-only decision; run their named focused tests | Each variant fails its runtime assertion; restore source and rebuild before final gates. |
| Local operational recovery | Installed settings `history stats`; encrypted backup/hash comparison; installed AI worker with synthetic stdin input | Active history, preserved original history bytes, and a nonempty successful response with the configured model. No private source text is sent for testing. |
| Regression and architecture | Workspace fmt/Clippy, CI-feature wrapped workspace tests, dependency rules, facade checks, process-clean | Checks pass, tests actually execute, and owned runners exit. |

## Evidence and limits

The existing encrypted history was copied and SHA-256 matched before the unowned empty lock was quarantined. The history bytes remained unchanged during recovery. The installed service subsequently reported active with zero dropped events and persistence failures. The cause of the orphan's creation remains unknown.

A bounded synthetic request reproduced HTTP 400 with parameter `service_tier`: the configured API project did not allow Priority. Explicitly changing the local preference to the provider-default tier returned HTTP 200. The installed worker then returned Applied with a nonempty result, the exact configured model, and one HTTP attempt. This is an operational configuration correction, not an automatic tier downgrade in the product.

The trace change addresses the synchronous diagnostic-I/O path tracked in #411. It does not establish the cause of every deadline expiry or complete #148's end-to-end key-loss work. The read-only gate covers hosts that report their status correctly; it cannot establish the intent or identity of a historical repeat-key burst. Live composition becoming read-only continues through the existing rejection/terminalization path.

Focused checks executed 51 tests, including the real-engine developer-mode trace lifecycle. Three behavior-removal mutations failed their runtime assertions as intended and were restored before the final build. The complete CI-feature workspace run passed 2,666 tests with 187 explicitly ignored and zero failures; repository-scoped runners exited. Formatting, Clippy with `-D warnings`, dependency policy (including its self-test), enforced architecture rules, and the facade gate passed.

The locked workspace release build passed. The release TSF DLL is 452,608 bytes against the 1,048,576-byte gate. Build and test processes were absent in the final repository-scoped process check. These build results do not mean the new binaries have been installed or released.

The IRV comparison passed the unconditional-document budget (20,749 / 24,576 bytes) and retained advisory baseline-growth warnings for engine key/candidate, core conversion, history, renderer, and CI scopes. It is not a claim that all architectural debt is resolved.

The installed CLI verified live recovery. A checkout-built CLI correctly rejected the installed engine's different image path at the existing trust boundary, so the new CLI freshness fields have not been exercised against that installed process. No trust policy was relaxed to make the probe pass.

## Follow-up: wait semantics and real-pipe regression

The previous three-second CLI check reported failure while a successfully started history generation was still initializing. Enabling now uses a 180-second observation budget with bounded exponential backoff and jitter; disabling retains three seconds. Each transport operation keeps its own existing deadline. The engine remains the initialization owner, and transport failure or observation expiry still returns nonzero. The CLI labels the result `unconfirmed`, describes the already-saved configuration, and directs the operator to `history stats`; it does not claim that inactive means failed or healthy.

The trace integration fixture is explicitly opt-in through the engine/IPC `dev-fixtures` feature and an argument bound to the private test-pipe namespace. It delays the actual writer before its file access, verifies no earlier key primed the fixture, checks two growing preedits plus a single final commit, and waits for a complete delayed trace row. The first focused run's maximum measured key reply was 289 microseconds with a 200-millisecond trace delay. This is regression evidence for that fixture, not a general latency benchmark.

A temporary mutation that made producers wait for trace completion caused the real-pipe test to fail with `Timeout`. Restoring a three-second enable budget caused the slow-initialization regression to fail. Both mutations were restored before final checks. The workspace run executed 2,670 passing tests with 187 explicitly ignored and zero failures.

Clippy then detected that `tools/ime-eval` also compiles the engine's `main.rs`, under a different package feature namespace. Fixture availability and its compile-time gate now belong to the IPC trace implementation instead of that shared entry point. After this correction, workspace/all-target Clippy with all CI features and `-D warnings` passed, and the 25 focused tests plus the default-feature startup rejection test passed again. The final focused real-pipe maximum key reply was 259 microseconds. Default workspace feature inspection proved the delay implementation is excluded from shipping features. Dependency policy/self-test, enforced architecture rules, facade, and the unconditional IRV documentation budget passed; the existing advisory IRV/architecture warnings remain.

The final locked workspace release build passed; the TSF DLL remains 452,608 bytes. The final process check found no repository-scoped build/test runner. This follow-up does not install or publish those binaries.

## IRV

The four findings required history lifecycle/storage, trace persistence, AI request/error handling, and TSF admission contracts. Scope expanded from logs to those four boundaries because the failures have separate owners; no general refactor was performed. One private trace-worker module isolates asynchronous ownership from existing rotation/backoff rules without adding crate dependencies or changing wire formats. The next investigation can begin with freshness fields, fixed API parameter codes, and this rubric; it must still distinguish producer-side history from host document application and trace stalls from other timeout causes.

The follow-up adds only CLI observation behavior and a private-pipe trace-delay fixture. Its scope expands into engine startup and the shared child harness because a unit sink cannot prove the real response path. History initialization ownership, host composition ownership, and wire contracts remain unchanged. The new test-only feature edge is engine to IPC and is absent from shipping defaults. The next similar investigation can reproduce the response/trace ordering directly without writing another process harness, and must understand the distinction between a saved preference, an observation timeout, and a failed initialization.
