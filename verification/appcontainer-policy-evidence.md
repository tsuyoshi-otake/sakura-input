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

## 2026-09-13 bounded native namespace support

CI runs 34704517571 and 34705378113 again rejected the owned engine before Hello. The later diagnostic reported a native-device path, expected DOS drive path, expected canonicalization OS 5 and observed canonicalization OS 3. The original admission value remains unobserved; these failures support a namespace hypothesis without proving historical causality.

The policy now translates a captured native-device path using only the anchor drive's current QueryDosDeviceW mapping. A single bounded lookup uses the first MULTI_SZ entry, never historical mappings. Exact device-prefix and separator matching, malformed-path rejection, and the existing canonical/reparse/layout/equality checks remain required. There is no process requery, retry, reconnect, token exception or new raw-path log. Unresolvable mappings reject. The pre-existing Exact lexical fallback remains test/diagnostic scope; InstalledRoot still requires filesystem validation.

Parent verification: five new unit tests cover mapping, prefix collision, malformed/NUL input, current-versus-historical MULTI_SZ termination, and actual Windows drive mapping through both policies with negative sibling/exact-path controls. IPC security tests, locked IPC all-target clippy with warnings denied, and the wrapped locked workspace tests passed. Repository process cleanup and diff checks passed. The real sandbox integration remains hosted-CI verification; local workspace success does not exercise ignored sandbox tests or establish that #104 is resolved.

### Follow-up: mapping proposal remains insufficient

Hosted PR #167 job 103586927461 failed with the same owned-PID rejection and later native-device shape. The ignored private-pipe AppContainer test passed locally, with scoped process cleanup, demonstrating an environment difference but not its cause. Added bounded test-only drive-mapping diagnostics to discriminate API failure, termination, mapping namespace and prefix/equality mismatch without printing paths. The diagnostic unit test and private sandbox test passed after this addition. No new admission fallback or retry was added; #104 remains open.

The following hosted job 103588260690 passed the original sandbox test. A deterministic native-image assertion was therefore added inside the real sandbox, after verified pipe admission and owned-PID equality but before Hello. This produced a local counterexample: `drive_mapping_query=error(code=HRESULT(0x80070005))`, `policy_recheck=false`. The proposed normalizer cannot query the DOS-device mapping under this AppContainer token. Diagnostic unit tests passed and owned test processes exited. The mapping proposal must be replaced; intermittent CI success is not sufficient evidence to merge it.
