# H2 endpoint reservation tranche (#120)

Main baseline: `f48d8f55fcab3f9abb210aa2a442cd8b217df1f1`, v1.0.35. Patch base: PR #119 (`8bb0288`).
Classification: initialization-before-ownership **CONFIRMED and fixed**; full H2 remains **MITIGATED**.

## Boundary

`main::run` now configures AppContainer process/token identity-query access, constructs the existing secured endpoint definitions, binds a validated private test name if requested, and reserves all required first pipe instances before opening dictionary, learning or history. `Server::reserve` owns these handles without starting threads. Failure in reservation drops earlier handles; later initialization failure drops the reserved Server. A second process fails the existing first-instance check before touching these stores.

After service configuration, the existing startup gate starts all endpoint workers transactionally. `run_when_ready` invokes the content-free Ready callback after the gate opens. `run` remains the existing convenience API. Production has three separate endpoints and admission pools; the private test path has one. Changing the pipe name after reservation is rejected. The SDDL, SID/logon resolution, first-instance flag, capacity and same-handle client identity contract are unchanged.

Per-logon engine ownership does not establish a per-user store writer lock. Other logons, old binaries, settings direct writes and crash recovery remain H2/H1 work. A reserved pipe can exist while runtime initialization is pending; readiness requires the protocol worker, and clients retain their existing timeouts. Watchdog rate limiting and elimination of redundant launch attempts are not implemented here. Early content-free diagnostic log/dump housekeeping in main still precedes reservation. The Ready notification describes successful worker startup, not a permanent liveness promise.

## Verification scope

The real-process regression launches only an owned engine on a unique private pipe and isolated LOCALAPPDATA, then starts a second owned binary with that same pipe and an absent dictionary. The old implementation exits 1 at dictionary initialization; the fixed implementation exits 2 for AlreadyRunning. Each child is waited or killed/waited by its owner, including assertion failure. This proves ordering, not actual production data loss.

Unit tests use three private names with the production endpoint descriptor paths. They verify reservation excludes a second claimant before any worker exists, Drop releases all names, a renderer collision releases the previously acquired data endpoint, and failed startup emits no Ready. Existing gated thread-spawn cleanup is retained.

Commands, exits, parsed counts and input/log hashes are generated into `engine-startup-ownership-results.json`. Logs remain in the local sibling evidence directory. Before regression: exit 101, one semantic failure. Fixed focused regression: exit 0, one pass. Reservation tests: exit 0, two passes.

Independent static review identified a missing partial thread-spawn failure test. A `cfg(test)` boundary now drops the unstarted closure as a failed Builder::spawn would; the test injects failure after 0, 1 and 2 prior workers. It verifies abort/join completion, zero endpoint/total slots, no worker-held Shared references, reacquisition of all private names and no Ready notification. This is deterministic fault injection, not actual OS thread resource exhaustion. The reviewer re-read the final patch and found no remaining actionable in-scope finding. Read-only was behavioral, not OS-enforced; tests were executed by the parent.

Final `cargo test --locked -p sakura-engine --lib startup_`: exit 0, 8 passed. Final `cargo test --locked --workspace`: exit 0, 1,778 passed, 84 ignored, 0 failed, 93 result summaries. `cargo fmt --all -- --check` and `git diff --check` exit 0. The scoped `ci/check-process-clean.ps1` check exits 0 with no surviving repository-owned runner after every test invocation. Hook absence from default/release is verified through cfg(test) source boundaries; no binary symbol/export scan is claimed.

Windows x64, Rust/Cargo 1.96.0, locked dependencies, debug profile. AppContainer ignored test, physical TSF, multi-logon/RDP, watchdog delayed-start matrix, crash/disk-full injection, H2-specific PBT/C2/mutation/TLC remain NOT_RUN. No installed IME, production pipe, real history, registration or release action was used.
