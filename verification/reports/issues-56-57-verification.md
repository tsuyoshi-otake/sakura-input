# Issues #56 and #57 verification and adversarial audit

Date: 2026-08-14

## Independent oracles, examples, PBT, and C2

The Shift-started ASCII oracle is a declarative four-field match table, separate
from the production branch order. Its fixed-seed campaign ran 4,096 cases and
an exhaustive tail ran all 16 Boolean combinations. The four atomic conditions
each took both values: C2 = 8/8 (100%). Concrete dispatch examples cover an
unknown word plus repeated plain spaces, a known term plus Shift+Space, the
existing known-term plain-Space conversion, and Japanese Shift+Space behavior.

The recovery-fence oracle stores only an optional abstract generation and
defines whether a callback matches it. Its fixed-seed campaign ran 4,096 cases.
The four condition occurrences in `begin`, disposition, `finish`, and cancel
each took true and false: C2 = 8/8 (100%). This is condition coverage, not a
claim of MC/DC or of C2 for every condition in `text_service.rs`.

Stable Rust 1.96 does not accept LLVM's nightly-only branch coverage option.
That attempted command failed before tests and is not counted as evidence.
Supplementary non-branch `cargo-llvm-cov` results were:

| Domain file | Functions | Lines | Regions |
| --- | ---: | ---: | ---: |
| `shift_ascii_space.rs` | 1/1 | 13/13 | 13/13 |
| `engine_recovery.rs` | 9/9 | 49/49 | 57/57 |

## Fake-API integration state transitions

`RecoveryApiHarness` connects the domain fence to a fake document API and a
bounded callback queue. Its examples verify:

| Boundary or transition | Observable expectation |
| --- | --- |
| Empty/idle boundary | No finalizer; key may go to the host |
| Synchronous apply | Document changes once; fence terminates before return |
| Request rejection | Explicit `Rejected`; no residual fence |
| Timeout/queued apply | Triggering and following keys are consumed |
| Retry while pending | One token and one queued write; retry is deduplicated |
| Duplicate or missing callback | No second document write |
| Lifecycle cancellation | Missing callback reaches explicit `Cancelled` |
| External document change | Pending work is cancelled before stale delivery |
| Old/new callback reversal | Old token cannot edit or release the new token |
| Recovery after terminal state | The next host delete is admitted normally |

The real TSF fence-decision regression additionally asserts that
`OnTestKeyDown` reports `Busy` and the real key path returns `Consume` while a
recovery finalizer remains pending.

## Mutation testing

`cargo-mutants` 27.1.0 evaluated the two domain files and the probe-fence
logical operators. Of 23 in-scope generated mutants, 16 were compilable and all
16 were caught; 7 generated `Default::default()` replacements for non-Default
return types and were unviable. There were zero in-scope survivors and zero
timeouts. Thus the viable-mutant kill rate is 16/16 (100%); unviable mutants are
not counted as killed.

The probe-fence invocation also forcibly generated six pre-existing unsafe
Windows struct-field deletion mutants outside the requested function filter.
The deliberately narrow `probe_fence` test filter did not catch those six. They
are excluded from the in-scope rate, but recorded here rather than hidden; no
claim is made about mutation adequacy for window-class or language-bar setup.

## TLA+/TLC correspondence

| Important boundary | Rust example/PBT | Fake API | TLC action/property | Implementation |
| --- | --- | --- | --- | --- |
| Timeout begins one fence | recovery examples/PBT | queued timeout | `Timeout`, `PendingBackedByCallback` | `begin_engine_recovery` |
| Keys cannot race old finalizer | real/probe fence regression | delete is consumed | `HostKey`, `PendingVersionIsCurrent` | `decide_real_fence`, `decide_probe_fence` |
| Retry is deduplicated | recovery PBT | second timeout keeps queue length one | timeout pending branch | `EngineRecoveryFence::begin` |
| Apply/reject/cancel are terminal | recovery examples | sync/reject/lifecycle cases | `TerminalAtMostOnce`, liveness | completion settlement/detach |
| Stale or reversed callback is harmless | generation example | reversed delivery | `CompleteAny`, `NoStaleReplay` | token-matching `finish` |
| Queue/resource bounds fail closed | recovery examples cover one slot | rejected request | `QueueBounded`, capacity branch | write-coordinator admission/rejection |
| Shift+Space is literal in English preedit | exhaustive Boolean oracle | n/a | n/a | `begin_conversion` decision |
| Plain Space preserves known conversion | dispatch regression | n/a | n/a | dictionary-hit fact |

TLC results, constants, state counts, depths, action reachability, fairness, and
unexplored configurations are recorded in `verification/tla/README.md`.

## Adversarial residual-risk assessment

- The input-history file contained no decryptable key records for this event;
  privacy filtering had excluded 1,930 unclassified inputs. A diagnostic ring
  entry proves one VS Code key timeout at 2026-08-14 22:14:36.854 JST, but does
  not prove which physical key timed out. The stale-finalizer race is therefore
  a supported failure hypothesis, not a post-mortem proof of the exact incident.
- The fake API and TLA+ model cannot construct a real Electron/VS Code
  `ITfContext`. Existing source and regression checks retain the Electron-safe
  `GetSelection` path and prohibit returning to `ITfInsertAtSelection`, but a
  live VS Code soak test and crash-dump confirmation remain necessary to close
  host-specific COM and scheduler risk.
- PBT abstracts the recovery payload to generations and Shift-space inputs to
  four Boolean facts. It does not enumerate Unicode text, every keymap profile,
  or every Windows input-scope transition.
- C2 is oracle-instrumented because stable LLVM branch coverage is unavailable;
  it does not establish path coverage for the full TSF callback graph.
- Mutation results establish detection for compilable mutations in the scoped
  domain decisions, not the whole workspace. Seven in-scope unviable mutants
  and six explicitly excluded unsafe-struct mutants remain outside the kill
  denominator.
- TLC is exhaustive only within the recorded constants and fairness
  assumptions. Larger timed-out configurations, unfair scheduling, process
  loss, real time, memory allocation failure, and actors greater than two are
  unverified or unmodeled.

Within those limits, the independent examples, PBT, fake-API transitions,
mutation results, and completed TLC state spaces agree on the two critical
properties: a pending old-composition finalizer never admits a host edit, and a
stale completion never releases a newer recovery generation.
