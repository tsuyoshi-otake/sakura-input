# AppContainer image-policy evidence (#104, O1)

Baseline main `f48d8f55fcab3f9abb210aa2a442cd8b217df1f1`, v1.0.35. Patch base #128 (`eae7127`). Classification: **IMPROVEMENT** for diagnostics; intermittent CI root cause remains **HYPOTHESIS**.

PR #126 run 33968517317 attempt 1 failed in Sandbox access (AppContainer). The reason was ImagePathRejected, and the rejected PID printed in the log equaled the test-owned engine PID. One same-commit rerun passed Build and test in 8m27s. The original failure log is retained at the evidence sibling as `stop-ci-appcontainer-failure.log`. This is an observed failed policy decision, not proof of a different executable, nor proof that the shutdown patch caused or fixed it.

The existing Exact policy can reject lexical shape/equality, with canonicalization/reparse checks influencing its path. A boolean policy rejection does not expose which condition failed. The former categorical panic/comment interpretation therefore overstated the evidence.

On rejection, the test now logs whether the rejected PID matches the owned PID, a bounded read-only follow-up image query status, absolute/parent/verbatim/forward-slash/engine-name flags, case-insensitive lexical equality, plain canonicalization status and OS errors, canonical equality, and policy re-evaluation. It emits no raw path. Plain canonicalization diagnostics do not replace the policy's reparse checks. The later query may observe a different state from original admission; the panic says so explicitly.

This code exists only in the test executable. The real trust decision, accepted paths, Hello ordering and rejected-handle cleanup are unchanged. No diagnostic result authorizes reconnect or protocol traffic. The follow-up process handle is closed on success or query failure. No global configuration or AppContainer profile is changed by the new unit test.

Verification: `cargo test --locked -p sakura-engine --test appcontainer image_policy_diagnostics` passed one test after correcting missing OsString/OsStringExt imports. That first compilation failure is not an old-code semantic counterexample. The test verifies actual read-only querying of its own process in the final workspace run, output fields and path-label omission with synthetic inputs. `cargo clippy --locked --workspace --all-targets -- -D warnings` passed (`appcontainer-diagnostics-clippy.log`). Real sandboxed rejection diagnostics have not yet executed in CI; a passing admission will not exercise the new rejection branch.

Final `cargo test --locked --workspace` exited 0: 1,800 passed / 84 ignored / 0 failed (`workspace-policy-evidence.log`). Format, diff and scoped process-clean checks exited 0. No runner survived. Hashes/run scope are in the adjacent results JSON.

Independent adversarial static review passed the unchanged trust/Hello boundary, read-only handle ownership and diagnostic content checks. The reviewer ran no tests or edits; requested read-only behavior was not enforced by the OS.

Windows x64, Rust/Cargo 1.96.0, debug, locked dependencies. No local AppContainer integration/production well-known-pipe test was run. Full #104/O1 and causal attribution remain unfinished; no trust check was weakened to turn CI green.
