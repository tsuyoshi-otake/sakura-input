# T1 shared callback IPC deadline (#134)

Baseline main f48d8f55fcab3f9abb210aa2a442cd8b217df1f1 (v1.0.35); patch basec089ce7 (#133). Serial allowance renewal is **CONFIRMED** with a private scripted pipe, independent from the physical #102/#107 symptoms. Partial-response timeout classification is also confirmed at a synthetic boundary; it is not claimed as the historical #102 cause.

OnTestKeyDown, OnKeyDown and OnPreservedKey establish a thread-local RAII deadline. Nested COM reentry inherits the earlier expiry; scope exit restores the previous deadline. The guard cannot cross threads. Engine reconnect/handshake, resync, scope, key, administration and UI calls use the earlier of the callback deadline and the operation cap. Replacing the Engine during a callback cannot renew the allowance. Callbacks without this scope retain existing local caps; key-up has no engine IPC.

Client::call_until accepts an absolute Instant, with the existing relative call API delegating to it. Fault::DeadlineExpired means no request was sent, unlike Fault::Timeout after an issued request. Expiry between the final request check and first WriteFile restores the unissued request ID. Engine handles known pre-send expiry without inventing a session mutation. The settings fault adapters preserve content-free failure detail while mapping both expiry forms to TimedOut.

Transfer checks prevent starting another read/write after expiry. Once a response header or partial frame has been consumed, timeout is Desynchronized, so TSF retires the unaligned stream. A silent response timeout with no consumed bytes keeps the existing late-reply policy. Cancellation still waits for kernel buffer ownership to end. Only confirmed ERROR_OPERATION_ABORTED with zero transferred bytes is an ordinary cancellation timeout; completion racing cancellation retires the stream conservatively.

## Counterexamples and checks

| Verify | Expect / observed |
|---|---|
| `cargo test --locked -p sakura-tsf --lib callback_deadline_serial -- --nocapture` with old production path | Exit101, all resync/scope/key accepted despite combined110.0786ms (`deadline-before.log`) |
| Same first fixed path | Exit0, key not sent;56.1863ms (`deadline-after.log`) |
| `cargo test --locked -p sakura-tsf --lib callback_deadline -- --nocapture` | 3 passed; nested restoration, expired no-send/link preservation, serial cap; later intermediate sample62.192ms (`deadline-targeted.log`) |
| Partial reply test before framing correction | Exit101: prefix1 produced ordinary Timeout (`deadline-partial-before.log`); this run contains the initial deadline patch, not immutable main |
| `cargo test --locked -p sakura-ipc` after review corrections | 28 unit +7 integration passed (`deadline-ipc-reviewed.log`), including prefixes1/4/7, stale complete reply discard, silent timeout, cancellation race and final-check/write expiry |
| `cargo clippy --locked --workspace --all-targets -- -D warnings` | Exit0 (`deadline-reviewed-clippy.log`) |
| `cargo test --locked --workspace` | 1,818 passed /84 ignored /0 failed (`workspace-deadline-reviewed.log`) |
| `cargo fmt --all -- --check`; `git diff --check`; repository process-clean script | Exit0, no owned test survivors |

The serial before/after values are single debug samples with actual scheduler/diagnostic overhead; they are not p50/p95/p99, hard50ms bounds or a physical callback performance qualification. The regression asserts no later key success after serial expiry. Tests own private pipes and join peers. The callback entry wiring, including preserved keys, is statically inspected; no real ThreadMgr/host callback was invoked here.

Independent review found the intermediate transfer precheck still classified a no-send race as uncertain Timeout and missed OnPreservedKey. Both were corrected and the transport boundary regression added. Initial compile checking also found two exhaustive settings match arms needing the new variant; later Clippy required moving the test module after production items. These tooling failures are not semantic counterexamples.

Final independent re-review PASS for the corrected behavioral scope. Reviewer was behaviorally read-only, not OS-enforced; parent owned all execution. Final full checks follow production/test corrections and precede only a documentation clarification that partial-frame timeouts retire the stream. Final hashes and earlier-iteration logs are distinguished in the results JSON.

## Limits

No hard bound is claimed for arbitrary synchronous COM calls, executable identity filesystem checks, OS scheduling, diagnostic I/O or cancellation completion. All buffer/OVERLAPPED lifetimes remain owned until kernel completion; this patch does not shorten that required wait. Cancellation completion and partial reply are exercised, but partial request writes are covered by the shared code path rather than a separate saturating-pipe injection.

DeadlineExpired extends the public Fault enum; downstream exhaustive matches need an arm. Relative call callers now receive this variant if their allowance expires before sending, and local encode failures no longer consume a request ID. There is no wire schema, trust policy, dependency, scope or input-content logging change. TLC, real TSF, physical/ETW attribution, shipped resource qualification and deferred UI publication remain unfinished. Full T1/T4/V2 and the program are IN PROGRESS.
