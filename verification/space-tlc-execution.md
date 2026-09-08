# Space TLC execution and historical verdict containment (#106)

Fixed program baseline: `f48d8f55fcab3f9abb210aa2a442cd8b217df1f1`.
Patch base: `1d102961e2d9a7f2628a047a519ca9a9ef9b6d52`.
Classification: **CONFIRMED** verification-runner false success; **STALE**
historical product verdict. This is a bounded part of V1; #106 remains open.

The old script returned exit 0 after Java returned exit 1 for an invalid jar
(`ClassNotFoundException: tlc2.TLC`). A Java/TLC failure could therefore be
consumed as a successful verification command. Timeout also continued without
a failing final exit. The parent executed this counterexample before editing
the runner; local log `tlc-runner-before.log` preserves both exit observations.

The runner now validates the reviewed jar hash and exact configuration names,
uses a new run directory without deleting previous evidence, snapshots the jar,
model and configuration inputs before launching TLC, and emits `results.json` from
the actual processes. It records source revision **plus working-tree input
hashes**, Java/TLC version, seed, workers, bound, exit, timeout, state counts,
process termination and raw log hashes. Revision alone is not a claim that the
working tree was committed or clean. The jar snapshot is hash-checked after
copying and used for every configuration, so replacing the external jar cannot
silently change later runs. Model/config snapshots are consumed by TLC;
the copied runner is provenance, not a second execution of the script.

A safety configuration must report completed model checking, a positive state
count and no remaining queue with exit 0. A reachability configuration must
return exit 12, name the expected violated invariant and include a state trace.
An arbitrary nonzero exit is not a successful negative control. Timeout is
INCONCLUSIVE and makes the runner fail. Partial campaigns only claim the
configurations explicitly listed in their result, never all nine by inference.
The source files are checked again at the end; persistent input changes produce
STALE. Each Java process is awaited or its owned tree is terminated on timeout,
then awaited before disposal. This is execution evidence, not a sandbox against
a malicious local editor, Java runtime or kernel.

`space-key-dispatch/historical/traceability-4c7113c.json` preserves the old JSON
content (newline encoding may differ). Its actual evaluated revision is
**unknown**, not guessed from the commit that introduced the document. The old
NO_GO, missing-fence descriptions and old mutation/C2 denominators are explicitly
historical. `space-key-dispatch/traceability.json` now has schema 2, STALE status,
and NOT_RUN product conformance. Consumers must not expect the old flat verdict
schema. A warning at the start of the historical audit prevents its table from
being quoted as current findings.

## Executable rubric

| Verify | Expect | Observed |
|---|---|---|
| Old runner with the synthetic invalid jar | Reproduce incorrect success | Java exit 1, runner exit 0 |
| `pwsh -NoProfile -File scripts/verify-space-key-dispatch-tlc.ps1 -SelfTest` | Reject failure, missing summary, zero states, incomplete queue, wrong counterexample/exit, missing trace and timeout; preserve child exit and join timeout child | 12 outcome controls + 2 real child-process probes passed |
| Runner with invalid jar and a `../` configuration name | Reject before TLC execution | Both exit 1 |
| Actual boundary TLC run with a one-second timeout | Persist INCONCLUSIVE, return nonzero, terminate Java | Observed runner exit 1 and joined Java; `space-key-dispatch/tlc-timeout-results.json` |
| Runner with the pinned jar, defaults | Five completed bounded searches + four named reachability witnesses | See generated `space-key-dispatch/tlc-runner-results.json` |
| Task-scoped Java process inventory, then `ci/check-process-clean.ps1` | No owned Java/test/Sakura survivor | Clean after runs |
| `cargo test --locked --workspace` | No regression | 1,818 passed, 84 ignored, 0 failed; ignored tests were not executed |
| `cargo fmt --all -- --check`; `cargo clippy --locked --workspace --all-targets -- -D warnings`; `git diff --check` | Exit 0 | Passed |

Reproduction uses PowerShell 7, Microsoft OpenJDK 11.0.31, TLC 2.19, two workers,
seed 20260816, and the existing jar SHA-256
`936a262061c914694dfd669a543be24573c45d5aa0ff20a8b96b23d01e050e88`.
No dependency package was adopted. The workspace checks use Rust/Cargo 1.96.0,
Windows x64 MSVC, debug profile and the existing lockfile.

```powershell
pwsh -NoProfile -File scripts/verify-space-key-dispatch-tlc.ps1 `
  -JarPath C:/Users/developer/tmp/tla-tools-1.7.4/tla2tools.jar `
  -OutputRoot C:/Users/developer/tmp/sakura-revalidation-evidence-20260905/tlc-final
```

The command prints the unique result directory. Local raw logs are retained in
the sibling `sakura-revalidation-evidence-20260905` directory; generated results
carry their hashes, but those local logs are not published CI artifacts. The CI
addition runs the script's self-test, **not** Java model checking, PBT/mutation,
or a full dependency-closure freshness gate.

## Unfinished evidence and semantic correspondence

- The existing TLA model has no teardown credit. The requirements exclude
  orderly replace from REQ-SPACE-09, while oracle `ReplaceContext` arms credit.
  Resolving this correspondence and adding non-vacuous teardown checks remains
  necessary. No model or product behavior is silently changed in this patch.
- Existing liveness permits reaching `MaxEvents` as its terminal alternative.
  Completing that finite model is not proof of unbounded runtime termination.
  `CrashRestoresIdle == TRUE` is not meaningful crash verification.
- Current PBT/C2/mutation campaign manifests, production/test/toolchain/artifact
  dependency closure and scope-omission negative tests remain to be connected.
  Workspace test success does not replace that campaign or validate old counts.
- Real TSF/COM, physical routing, #69, installed/shipped artifacts, ETW causality
  and release qualification remain separate, unexecuted tiers for this patch.
- Independent adversarial review is behaviorally read-only, not enforced by the
  OS. The parent owns all executable verification and checks the actual diff.
